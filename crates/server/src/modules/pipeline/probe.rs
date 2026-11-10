//! The health probe: every `GUARDIAN_MODULE_PROBE_INTERVAL_MS` it asks each enabled module
//! whether it is up, half-opens or holds open its breaker, reruns the startup checks of an
//! instance that started unchecked, and publishes the state gauges.

use std::sync::Arc;

use super::{ModulePipeline, Rows};
use crate::modules::breaker::CircuitState;
use crate::modules::registry::Unhealthy;

/// How often the probe loop recounts the over-budget accounts and orphaned rows.
const ASSIGNMENT_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

impl ModulePipeline {
    /// Sets the state gauges from the stored rows and this replica's breakers. Run on every probe,
    /// so they are set once a metrics recorder exists and follow changes other replicas make.
    fn publish(&self, rows: &Rows) {
        crate::modules::metrics::catalog(&self.catalog_hash);
        let (over_budget, orphaned) = self
            .assignment_report
            .lock()
            .map(|report| *report)
            .unwrap_or_default();
        crate::modules::metrics::stored_assignments(over_budget, orphaned);
        for instance in &self.instances {
            let id = &instance.config.id;
            let enabled = Self::enabled_in(rows, instance);
            crate::modules::metrics::enabled(id, enabled);
            crate::modules::metrics::config_drift(
                id,
                enabled != instance.config.catalog_enabled || self.seed_drift.contains(id),
            );
            crate::modules::metrics::circuit_open(
                id,
                self.breakers.state(id) == CircuitState::Open,
            );
        }
    }

    /// One health probe of every enabled instance: `Describe` under its timeout, with the startup
    /// checks. A healthy module half-opens an open breaker so the next real call is sent. One
    /// that does not answer counts as a failed call toward its breaker. One that answers with a
    /// protocol version, capability or hook set this Guardian cannot work with opens its breaker
    /// at once: its decisions are not trusted until a probe passes again.
    pub async fn probe(&self) {
        let Ok(rows) = self.rows().await else {
            return;
        };
        self.publish(&rows);
        for instance in &self.instances {
            if !Self::enabled_in(&rows, instance) {
                continue;
            }
            let id = instance.config.id.as_str();
            let unchecked = self
                .unchecked
                .lock()
                .is_ok_and(|unchecked| unchecked.contains(id));
            // An instance that started without its startup checks runs all of them here, the
            // params that apply included, before any call is let through.
            let healthy = if unchecked {
                crate::modules::registry::check(
                    &instance.config,
                    &instance.module,
                    &Self::global_params_in(&rows, instance),
                )
                .await
                .map(|_| ())
            } else {
                crate::modules::registry::describe_healthy(&instance.config, &instance.module)
                    .await
                    .map(|_| ())
            };
            crate::modules::metrics::up(id, healthy.is_ok());
            match healthy {
                Ok(()) => {
                    if unchecked {
                        if let Ok(mut unchecked) = self.unchecked.lock() {
                            unchecked.remove(id);
                        }
                        tracing::info!(
                            module = id,
                            "Policy module passed the startup checks it could not be reached for"
                        );
                    }
                    self.breakers.probe_succeeded(id);
                }
                Err(Unhealthy::Unreachable(error)) => {
                    tracing::warn!(module = id, %error, "Policy module health probe failed");
                    self.breakers.failed(id);
                }
                Err(Unhealthy::Incompatible(error) | Unhealthy::InvalidParams(error)) => {
                    tracing::warn!(module = id, %error, "Policy module cannot be used by this Guardian; its circuit is open until a probe passes");
                    self.breakers.hold_open(id);
                }
            }
        }
    }

    /// Runs [`Self::probe`] every `GUARDIAN_MODULE_PROBE_INTERVAL_MS` for the life of the server.
    pub fn spawn_probe(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.limits.probe_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut counted = tokio::time::Instant::now();
            loop {
                interval.tick().await;
                if counted.elapsed() >= ASSIGNMENT_REPORT_INTERVAL {
                    self.refresh_assignment_report().await;
                    counted = tokio::time::Instant::now();
                }
                self.probe().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use guardian_module_sdk::HookRequest;

    use super::super::*;
    use crate::modules::breaker::CircuitState;
    use crate::modules::catalog::{HookName, OnError};
    use crate::modules::testing::{Behaviour, StubModule, instance, pipeline};

    const HOOK: HookName = HookName::BeforeTransactionApproved;

    fn hanging(id: &str, on_error: OnError, timeout: Duration) -> (Instance, Arc<StubModule>) {
        let module = StubModule::new(Behaviour::Hang);
        let mut hung = instance(id, &[HOOK], on_error, module.clone());
        hung.config.timeout = timeout;
        (hung, module)
    }

    async fn run(pipeline: &ModulePipeline) -> Result<(), GuardianError> {
        pipeline
            .run(HOOK, "0xacc", || Ok(HookRequest::default()))
            .await
    }

    #[tokio::test]
    async fn a_probe_publishes_the_state_gauges() {
        let pipeline = pipeline(vec![instance(
            "caps",
            &[HookName::BeforeTransactionApproved],
            OnError::Deny,
            StubModule::allowing(),
        )]);
        let rows = pipeline.rows().await.unwrap();
        let recorder = crate::metrics::build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || pipeline.publish(&rows));
        let text = handle.render();
        for line in [
            "guardian_module_enabled{module=\"caps\"} 1",
            "guardian_module_circuit_open{module=\"caps\"} 0",
            "guardian_module_config_drift{module=\"caps\"} 0",
            "guardian_module_over_budget_accounts 0",
            "guardian_module_orphaned_assignments 0",
            "guardian_module_catalog_info{catalog_hash=\"\"} 1",
        ] {
            assert!(text.contains(line), "{line} missing from:\n{text}");
        }
    }

    #[tokio::test]
    async fn a_module_that_turns_incompatible_is_not_trusted_until_a_probe_passes() {
        let mut stub = crate::modules::testing::ProtocolStub::new();
        stub.protocol_version = guardian_module_sdk::PROTOCOL_VERSION + 1;
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            Arc::new(stub),
        )]);
        run(&pipeline)
            .await
            .expect("the module allows until a probe finds it incompatible");
        pipeline.probe().await;
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleUnavailable { .. })
        ));
        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Open,
            "it stays open while the probe keeps failing"
        );
    }

    #[tokio::test]
    async fn probes_a_module_does_not_answer_count_toward_its_breaker() {
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            Arc::new(Undescribed),
        )])
        .with_limits(Limits {
            breaker_failures: 2,
            ..Limits::default()
        });
        pipeline.probe().await;
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Closed);
        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Open,
            "no request had to find the module down"
        );
    }

    #[tokio::test]
    async fn a_healthy_probe_half_opens_and_a_failing_one_leaves_the_circuit_open() {
        let healthy = crate::modules::testing::instance_with_params(
            "caps",
            &[HOOK],
            Arc::new(guardian_module_sdk::policies::RecipientPolicy),
            serde_json::json!({}),
        );
        let (down, _) = hanging("screening", OnError::Deny, Duration::from_millis(20));
        let pipeline = ModulePipeline::from_instances(vec![healthy, down]).with_limits(Limits {
            breaker_failures: 1,
            ..Limits::default()
        });
        pipeline.breakers().failed("caps");
        pipeline.breakers().failed("screening");
        pipeline.probe().await;
        assert_eq!(pipeline.breakers().state("caps"), CircuitState::HalfOpen);
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
    }

    /// Allows every request but never answers `Describe`.
    struct Undescribed;

    #[async_trait::async_trait]
    impl guardian_module_sdk::Module for Undescribed {
        async fn describe(
            &self,
            _: &guardian_module_sdk::DescribeRequest,
        ) -> Result<guardian_module_sdk::ModuleInfo, guardian_module_sdk::ModuleError> {
            Err(guardian_module_sdk::ModuleError::Unavailable(
                "no answer".into(),
            ))
        }

        async fn validate_params(
            &self,
            _: &[u8],
        ) -> Result<guardian_module_sdk::ParamsVerdict, guardian_module_sdk::ModuleError> {
            Ok(guardian_module_sdk::ParamsVerdict::ok())
        }

        async fn evaluate(
            &self,
            _: &HookRequest,
        ) -> Result<guardian_module_sdk::Decision, guardian_module_sdk::ModuleError> {
            Ok(guardian_module_sdk::Decision::allow())
        }
    }
}

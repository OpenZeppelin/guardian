use crate::config::SchemeDistribution;
use crate::model::AuthScheme;

pub fn build_scheme_plan(user_count: u32, distribution: &SchemeDistribution) -> Vec<AuthScheme> {
    if user_count == 0 {
        return Vec::new();
    }

    let mut falcon_users = user_count * u32::from(distribution.falcon_percent) / 100;
    let mut ecdsa_users = user_count.saturating_sub(falcon_users);

    if distribution.falcon_percent > 0 && falcon_users == 0 && user_count > 0 {
        falcon_users = 1;
        ecdsa_users = user_count.saturating_sub(falcon_users);
    }
    if distribution.ecdsa_percent > 0 && ecdsa_users == 0 && user_count > 1 {
        ecdsa_users = 1;
        falcon_users = user_count.saturating_sub(ecdsa_users);
    }

    let mut plan = Vec::with_capacity(user_count as usize);
    for _ in 0..falcon_users {
        plan.push(AuthScheme::Falcon);
    }
    for _ in 0..ecdsa_users {
        plan.push(AuthScheme::Ecdsa);
    }

    while plan.len() < user_count as usize {
        plan.push(AuthScheme::Falcon);
    }

    plan
}

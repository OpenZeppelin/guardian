// Stub for the optional @getpara/aa-* account-abstraction provider packages.
//
// @getpara/react-sdk-lite lazily `await import("@getpara/aa-<provider>")` inside its
// smart-account hooks, declaring each as an OPTIONAL peerDependency. The multisig smoke
// harness never calls those hooks, and the packages are not installed. Without this stub
// Vite fails to resolve the dynamic specifiers (dep optimization + import-analysis).
//
// The throw is reached only if one of those AA hooks is actually invoked, in which case
// it surfaces the same "provider not installed" failure the SDK's own try/catch expects.
throw new Error(
  'Optional @getpara account-abstraction provider is not installed (stubbed for the multisig smoke harness).',
);

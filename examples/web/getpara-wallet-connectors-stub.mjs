// Stub for the optional @getpara/{evm,solana,cosmos}-wallet-connectors packages, which are not
// installed here.
//
// @getpara/react-sdk-lite lazily imports each connector on mount, so without this stub Vite cannot
// resolve the specifiers and the build fails. The stub is deliberately empty rather than throwing:
// `inlineDynamicImports` hoists it into the single chunk as eager top-level code, so a throw would
// fire uncaught on page load instead of being caught by the SDK. The SDK guards every access on the
// imported namespace, so an empty namespace leaves the connector unregistered and children render
// unchanged.
export {};

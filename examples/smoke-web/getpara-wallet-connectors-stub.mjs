// Stub for the optional @getpara/{evm,solana,cosmos}-wallet-connectors packages, which are not
// installed here.
//
// @getpara/react-sdk-lite lazily imports each connector on mount, so without this stub Vite cannot
// resolve the specifiers and the build fails. Throwing is what the SDK's own try/catch treats as an
// uninstalled connector: it falls through to its stub context and children render unchanged.
throw new Error('Optional @getpara wallet connectors are not installed (external wallets unsupported here).');

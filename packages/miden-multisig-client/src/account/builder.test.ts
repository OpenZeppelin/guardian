import { beforeEach, describe, expect, it, vi } from "vitest";
import { createMultisigAccount, validateMultisigConfig } from "./builder.js";
import { PROCEDURE_ROOTS } from "../procedures.js";

const {
  withSupportsAllTypes,
  createAuthGuardedMultisig,
  MockAuthGuardedMultisigConfig,
  MockAccountBuilder,
} = vi.hoisted(() => {
  const withSupportsAllTypes = vi.fn((component) => component);
  const createAuthGuardedMultisig = vi.fn((config) => ({
    config,
    withSupportsAllTypes: () => withSupportsAllTypes({ config }),
  }));
  class MockAuthGuardedMultisigConfig {
    constructor(
      public approvers: unknown[],
      public threshold: number,
      public guardian: unknown,
      public scheme: unknown,
    ) {}
    withProcThresholds(thresholds: unknown[]) {
      return Object.assign(
        new MockAuthGuardedMultisigConfig(
          this.approvers,
          this.threshold,
          this.guardian,
          this.scheme,
        ),
        { thresholds },
      );
    }
  }

  class MockAccountBuilder {
    accountType() {
      return this;
    }

    storageMode() {
      return this;
    }

    withAuthComponent() {
      return this;
    }

    withComponent() {
      return this;
    }

    withBasicWalletComponent() {
      return this;
    }

    build() {
      return {
        account: { id: () => ({ toString: () => "0x" + "a".repeat(30) }) },
      };
    }

    buildWithoutSchemaCommitment() {
      return {
        account: { id: () => ({ toString: () => "0x" + "a".repeat(30) }) },
      };
    }
  }

  return {
    withSupportsAllTypes,
    createAuthGuardedMultisig,
    MockAuthGuardedMultisigConfig,
    MockAccountBuilder,
  };
});

vi.mock("@miden-sdk/miden-sdk", () => ({
  AccountBuilder: MockAccountBuilder,
  AuthGuardedMultisigConfig: MockAuthGuardedMultisigConfig,
  AuthScheme: { AuthEcdsaK256Keccak: 1, AuthRpoFalcon512: 2 },
  ProcedureThreshold: class {
    constructor(
      public procRoot: unknown,
      public threshold: number,
    ) {}
  },
  Word: { fromHex: (hex: string) => ({ hex }) },
  createAuthGuardedMultisig,
  AccountStorageMode: {
    public: () => "public",
    private: () => "private",
  },
}));

describe("createMultisigAccount", () => {
  beforeEach(() => {
    vi.stubGlobal("crypto", {
      getRandomValues(buffer: Uint8Array) {
        return buffer;
      },
    });
    withSupportsAllTypes.mockClear();
    createAuthGuardedMultisig.mockClear();
  });

  function makeClient() {
    const webClient = {
      accounts: {
        insert: vi.fn().mockResolvedValue(undefined),
      },
    };
    return { webClient };
  }

  it("builds the guarded component from the upstream standard component (Falcon)", async () => {
    const { webClient } = makeClient();

    await createMultisigAccount(
      webClient as never,
      {
        threshold: 1,
        signerCommitments: ["0x" + "1".repeat(64)],
        guardianCommitment: "0x" + "2".repeat(64),
      },
      "http://localhost:57291",
    );

    // The component must come from the SDK, not from MASM compiled here: a locally compiled
    // component links the standards package dynamically and yields an `auth_tx` root the client
    // cannot classify, so it attaches no fee conversion info and the transaction fails.
    expect(createAuthGuardedMultisig).toHaveBeenCalledTimes(1);
    expect(createAuthGuardedMultisig.mock.calls[0][0].scheme).toBe(2);
    expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
  });

  it("passes the ECDSA scheme through to the component", async () => {
    const { webClient } = makeClient();

    await createMultisigAccount(
      webClient as never,
      {
        threshold: 1,
        signerCommitments: ["0x" + "1".repeat(64)],
        guardianCommitment: "0x" + "2".repeat(64),
        signatureScheme: "ecdsa",
      },
      "http://localhost:57291",
    );

    // The scheme is no longer implicit in the MASM — it is an explicit parameter, and the
    // wallet is an ECDSA consumer, so passing it wrongly would silently build Falcon accounts.
    expect(createAuthGuardedMultisig).toHaveBeenCalledTimes(1);
    expect(createAuthGuardedMultisig.mock.calls[0][0].scheme).toBe(1);
    expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
  });
  it("keys per-procedure thresholds by the pinned root, not the component export name", async () => {
    const { webClient } = makeClient();

    await createMultisigAccount(
      webClient as never,
      {
        threshold: 2,
        signerCommitments: ["0x" + "1".repeat(64), "0x" + "2".repeat(64)],
        guardianCommitment: "0x" + "9".repeat(64),
        procedureThresholds: [
          { procedure: "send_asset", threshold: 2 },
          { procedure: "update_signers", threshold: 2 },
        ],
      } as never,
      "http://localhost",
    );

    // `send_asset` and `update_signers` are this package's names. The component exports
    // `move_asset_to_note` (on BasicWallet, not the auth component at all) and
    // `update_signers_and_threshold` — so a root read off the component would be wrong or throw.
    const built = createAuthGuardedMultisig.mock.calls.at(-1)?.[0] as {
      thresholds: Array<{ procRoot: { hex: string }; threshold: number }>;
    };
    expect(built.thresholds.map((t) => t.procRoot.hex)).toEqual([
      PROCEDURE_ROOTS.send_asset,
      PROCEDURE_ROOTS.update_signers,
    ]);
  });
});

describe("validateMultisigConfig", () => {
  const signer = "0x" + "1".repeat(64);

  it("rejects a guardian commitment equal to a signer (matches upstream Rust invariant)", () => {
    expect(() =>
      validateMultisigConfig({
        threshold: 1,
        signerCommitments: [signer],
        guardianCommitment: signer,
      }),
    ).toThrow(/different from all signer commitments/);
  });

  it("accepts a distinct guardian commitment", () => {
    expect(() =>
      validateMultisigConfig({
        threshold: 1,
        signerCommitments: [signer],
        guardianCommitment: "0x" + "2".repeat(64),
      }),
    ).not.toThrow();
  });

  describe("procedure threshold overrides vs update_procedure_threshold", () => {
    const signers = Array.from(
      { length: 5 },
      (_, i) => "0x" + String(i + 1).repeat(64),
    );
    const guardian = "0x" + "9".repeat(64);

    const config = (
      threshold: number,
      procedureThresholds: Array<{ procedure: string; threshold: number }>,
    ) =>
      ({
        threshold,
        signerCommitments: signers,
        guardianCommitment: guardian,
        procedureThresholds,
      }) as Parameters<typeof validateMultisigConfig>[0];

    it("rejects an override above the default threshold that guards the setter", () => {
      expect(() =>
        validateMultisigConfig(
          config(2, [{ procedure: "send_asset", threshold: 4 }]),
        ),
      ).toThrow(
        /exceeds the threshold of 2 that guards update_procedure_threshold/,
      );
    });

    it("accepts the same override once the setter is raised to match", () => {
      expect(() =>
        validateMultisigConfig(
          config(2, [
            { procedure: "send_asset", threshold: 4 },
            { procedure: "update_procedure_threshold", threshold: 4 },
          ]),
        ),
      ).not.toThrow();
    });

    it("rejects an override above an explicitly raised setter", () => {
      expect(() =>
        validateMultisigConfig(
          config(2, [
            { procedure: "send_asset", threshold: 4 },
            { procedure: "update_procedure_threshold", threshold: 3 },
          ]),
        ),
      ).toThrow(
        /exceeds the threshold of 3 that guards update_procedure_threshold/,
      );
    });

    it("accepts overrides at or below the default threshold", () => {
      expect(() =>
        validateMultisigConfig(
          config(3, [
            { procedure: "send_asset", threshold: 3 },
            { procedure: "receive_asset", threshold: 1 },
          ]),
        ),
      ).not.toThrow();
    });

    it("allows the setter override to exceed the default threshold", () => {
      // Raising only the setter is always safe: it makes overrides harder to
      // edit, never easier.
      expect(() =>
        validateMultisigConfig(
          config(2, [
            { procedure: "update_procedure_threshold", threshold: 5 },
          ]),
        ),
      ).not.toThrow();
    });
  });
});

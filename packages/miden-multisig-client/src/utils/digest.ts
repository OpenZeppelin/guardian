import type { RequestAuthPayload } from '@openzeppelin/guardian-client';
import { AccountId, Felt, FeltArray, Rpo256, Word } from '@miden-sdk/miden-sdk';

export class AuthDigest {
  static fromAccountIdWithTimestamp(accountId: string, timestamp: number): Word {
    const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
    const parsedAccountId = AccountId.fromHex(paddedHex);
    const prefix = parsedAccountId.prefix();
    const suffix = parsedAccountId.suffix();

    const feltArray = new FeltArray([
      prefix,
      suffix,
      new Felt(BigInt(timestamp)),
      new Felt(0n),
    ]);

    return Rpo256.hashElements(feltArray);
  }

  static fromRequest(accountId: string, timestamp: number, requestPayload: RequestAuthPayload): Word {
    return AuthDigest.fromAccountIdTimestampAndPayloadWord(
      accountId,
      timestamp,
      AuthDigest.payloadWordFromBytes(requestPayload.toBytes()),
    );
  }

  private static fromAccountIdTimestampAndPayloadWord(
    accountId: string,
    timestamp: number,
    payloadWord: Word,
  ): Word {
    const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
    const parsedAccountId = AccountId.fromHex(paddedHex);
    const prefix = parsedAccountId.prefix();
    const suffix = parsedAccountId.suffix();

    const feltArray = new FeltArray([
      prefix,
      suffix,
      new Felt(BigInt(timestamp)),
      ...payloadWord.toFelts(),
    ]);

    return Rpo256.hashElements(feltArray);
  }

  static fromCommitmentHex(commitmentHex: string): Word {
    const paddedHex = commitmentHex.startsWith('0x') ? commitmentHex : `0x${commitmentHex}`;
    const cleanHex = paddedHex.slice(2).padStart(64, '0');
    return Word.fromHex(`0x${cleanHex}`);
  }

  private static emptyPayloadWord(): Word {
    return Word.fromHex(`0x${'0'.repeat(64)}`);
  }

  private static payloadWordFromBytes(bytes: Uint8Array): Word {
    if (bytes.length === 0) {
      return AuthDigest.emptyPayloadWord();
    }

    const payloadElements: Felt[] = [];
    for (let i = 0; i < bytes.length; i += 8) {
      let packed = 0n;
      for (let j = 0; j < 8 && i + j < bytes.length; j += 1) {
        packed |= BigInt(bytes[i + j]) << (8n * BigInt(j));
      }
      payloadElements.push(new Felt(packed));
    }

    return Rpo256.hashElements(new FeltArray(payloadElements));
  }
}

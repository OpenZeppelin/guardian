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
      new Felt(BigInt(0)),
    ]);

    return Rpo256.hashElements(feltArray);
  }

  static fromCommitmentHex(commitmentHex: string): Word {
    const paddedHex = commitmentHex.startsWith('0x') ? commitmentHex : `0x${commitmentHex}`;
    const cleanHex = paddedHex.slice(2).padStart(64, '0');
    return Word.fromHex(`0x${cleanHex}`);
  }
}

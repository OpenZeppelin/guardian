import { RequestAuthPayload } from '@openzeppelin/guardian-client';
import { describe, expect, it } from 'vitest';

import { AuthDigest } from '../src/utils/digest.js';

// Cross-language parity with `a_tagged_json_payload_matches_the_typescript_client` in
// `crates/shared/src/auth_request_payload.rs`.
describe('request payload words', () => {
  it('matches the Rust payload word of an execution request tagged as ExecuteDeltaProposal', () => {
    const payload = RequestAuthPayload.forOperation('ExecuteDeltaProposal', {
      proposal_id: '0xproposal',
      account_id: '0xabc',
    });

    const felts = AuthDigest.payloadWord(payload)
      .toFelts()
      .map((felt) => felt.asInt());

    expect(felts).toEqual([
      8769230917640929323n,
      9267576103040187092n,
      9755682680803901249n,
      2629457888911836854n,
    ]);
  });
});

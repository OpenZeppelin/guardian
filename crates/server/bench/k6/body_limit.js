import http from 'k6/http';
import { check } from 'k6';

const baseUrl = __ENV.PSM_HTTP_URL || 'http://localhost:3000';

const largeDataBytes = Number(__ENV.K6_LARGE_BYTES || '1500000');
const largeData = 'x'.repeat(largeDataBytes);

const headers = {
  'Content-Type': 'application/json',
  'x-pubkey': '0x01',
  'x-signature': '0x01',
  'x-timestamp': '1',
};

function buildPayload(dataValue) {
  return JSON.stringify({
    account_id: '0x01',
    auth: {
      MidenFalconRpo: {
        cosigner_commitments: [],
      },
    },
    initial_state: {
      data: dataValue,
    },
  });
}

export const options = {
  vus: 1,
  iterations: 2,
  noConnectionReuse: true,
};

export default function () {
  const small = http.post(`${baseUrl}/configure`, buildPayload('ok'), { headers });
  const large = http.post(`${baseUrl}/configure`, buildPayload(largeData), { headers });

  if (__ENV.K6_DEBUG_BODY === '1') {
    console.log(`small_status=${small.status} small_error=${small.error || ''}`);
    console.log(`large_status=${large.status} large_error=${large.error || ''}`);
  }

  check(small, {
    'small payload processed': (r) => r.status > 0 && r.status !== 413,
  });

  check(large, {
    'large payload rejected':
      (r) =>
        r.status === 413 ||
        (r.status === 0 && (!!r.error || !!r.error_code)) ||
        (typeof r.body === 'string' && r.body.includes('length limit exceeded')),
  });
}

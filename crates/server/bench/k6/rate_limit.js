import http from 'k6/http';
import { check } from 'k6';
import { Rate } from 'k6/metrics';

const baseUrl = __ENV.PSM_HTTP_URL || 'http://localhost:3000';
const sawStatus429 = new Rate('rate_limit_seen_429');
const sawStatus200 = new Rate('rate_limit_seen_200');

export const options = {
  scenarios: {
    limit_probe: {
      executor: 'constant-arrival-rate',
      rate: Number(__ENV.K6_RATE || '400'),
      timeUnit: '1s',
      duration: __ENV.K6_DURATION || '20s',
      preAllocatedVUs: Number(__ENV.K6_PREALLOCATED_VUS || '50'),
      maxVUs: Number(__ENV.K6_MAX_VUS || '200'),
    },
  },
  thresholds: {
    rate_limit_seen_429: ['rate>0'],
    rate_limit_seen_200: ['rate>0'],
  },
};

export default function () {
  const response = http.get(`${baseUrl}/pubkey`);
  const is200 = response.status === 200;
  const is429 = response.status === 429;

  sawStatus200.add(is200);
  sawStatus429.add(is429);

  check(response, {
    'status is 200 or 429': () => is200 || is429,
  });
}

SELECT
  calls,
  total_exec_time,
  mean_exec_time,
  rows,
  left(query, 200) AS query_sample
FROM pg_stat_statements
ORDER BY total_exec_time DESC
LIMIT 25;

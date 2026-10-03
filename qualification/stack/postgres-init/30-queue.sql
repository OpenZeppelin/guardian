-- The candidate-queue scenarios need a GUARDIAN that queues chained
-- candidates, and the candidates they leave queued must not reach the shared
-- fixture account on the main server, so the queue server has its own database.
CREATE DATABASE guardian_queue OWNER guardian;

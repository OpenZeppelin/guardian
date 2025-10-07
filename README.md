# Private State Manager

## Running with Docker Compose

1. Copy `.env.example` to `.env` and add your AWS credentials:

```bash
cp .env.example .env
```

2. Edit `.env` with your AWS credentials:

```bash
AWS_ACCESS_KEY_ID=your_access_key_here
AWS_SECRET_ACCESS_KEY=your_secret_key_here
AWS_REGION=us-east-1

PSM_APP_BUCKET_PREFIX=your_app_bucket_prefix
PSM_READ_BUCKET_PREFIX=your_read_bucket_prefix
```

3. Start the server:

```bash
docker-compose up -d
```

View logs:

```bash
docker-compose logs -f
```

Stop services:

```bash
docker-compose down
```

The server will be available at `http://localhost:3000`

## AWS Configuration

The server requires AWS credentials to access S3. Configure credentials using one of these methods:

1. **Environment variables**:
   ```bash
   export AWS_ACCESS_KEY_ID=your_access_key
   export AWS_SECRET_ACCESS_KEY=your_secret_key
   export AWS_SESSION_TOKEN=your_token  # Optional
   ```

2. **AWS credentials file** (`~/.aws/credentials`):
   ```ini
   [default]
   aws_access_key_id = your_access_key
   aws_secret_access_key = your_secret_key
   ```

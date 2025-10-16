#!/bin/bash
set -e

# Verify reproducible build hash for PSM Server
# This script builds the server and displays the hash for cross-machine verification

echo "================================================"
echo "PSM Server - Reproducible Build Hash"
echo "================================================"
echo ""

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Default to linux/amd64 for consistency across machines
# Override with: PLATFORM=linux/arm64 ./verify-build-hash.sh
PLATFORM="${PLATFORM:-linux/amd64}"

echo -e "${BLUE}Target Platform:${NC} $PLATFORM"
echo "  (Override with: PLATFORM=linux/arm64 $0)"
echo ""

# Verify we're in a git repository
if ! git rev-parse --git-dir > /dev/null 2>&1; then
    echo -e "${RED}Error: Not in a git repository${NC}"
    exit 1
fi

# Get current git commit
GIT_COMMIT=$(git rev-parse HEAD)
GIT_SHORT=$(git rev-parse --short HEAD)
GIT_DIRTY=""
if ! git diff-index --quiet HEAD --; then
    GIT_DIRTY=" (uncommitted changes)"
    echo -e "${RED}Warning: You have uncommitted changes!${NC}"
    echo "For reproducible builds, commit your changes first."
    echo ""
fi

echo -e "${BLUE}Git Commit:${NC} $GIT_SHORT$GIT_DIRTY"
echo ""

# Verify Dockerfile exists
if [ ! -f "Dockerfile" ]; then
    echo -e "${RED}Error: Dockerfile not found in current directory${NC}"
    echo "Run this script from the repository root"
    exit 1
fi

# Build the Docker image
echo "Building server in Docker..."
echo ""
docker build --platform "$PLATFORM" -t psm-server-verify . --no-cache --progress=plain 2>&1 | grep -v "jemalloc" | grep -v "MADV_DONTNEED" | grep -v "QEMU"

# Extract binary to temp location
BUILD_DIR=$(mktemp -d)
trap "rm -rf $BUILD_DIR; docker rmi psm-server-verify 2>/dev/null || true" EXIT

docker create --name psm-verify-temp psm-server-verify > /dev/null
docker cp psm-verify-temp:/app/server "$BUILD_DIR/server"
docker rm psm-verify-temp > /dev/null

# Calculate hash and size
HASH=$(sha256sum "$BUILD_DIR/server" | awk '{print $1}')
SIZE=$(wc -c < "$BUILD_DIR/server")

echo ""
echo "================================================"
echo -e "${GREEN}Build Complete${NC}"
echo "================================================"
echo ""
echo -e "${BLUE}Platform:${NC} $PLATFORM"
echo -e "${BLUE}SHA256:${NC}   $HASH"
echo -e "${BLUE}Size:${NC}     $SIZE bytes"
echo -e "${BLUE}Commit:${NC}   $GIT_COMMIT"
echo ""
echo "To verify across machines:"
echo "  1. Ensure same git commit: git checkout $GIT_SHORT"
echo "  2. Use same platform: PLATFORM=$PLATFORM ./crates/server/tests/verify-build-hash.sh"
echo "  3. Compare the SHA256 hashes - they should match exactly"
echo ""

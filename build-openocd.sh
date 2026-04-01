#!/bin/bash
# Build OpenOCD 1.0 from source for ESP32-S3 USB JTAG support
# This bypasses the OpenOCD 0.12.0 descriptor bug

set -e

# Configuration
INSTALL_PREFIX="$HOME/.local/openocd-1.0"
BUILD_DIR="/tmp/openocd-build"

echo "Building OpenOCD 1.0 for ESP32-S3 USB JTAG support..."
echo "Installation prefix: $INSTALL_PREFIX"

# Create build directory
mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

# Clone OpenOCD repository if not already present
if [ ! -d "openocd" ]; then
    echo "Cloning OpenOCD repository..."
    git clone --depth 1 -b v0.12.0 \
        https://git.code.sf.net/p/openocd/code openocd
fi

cd openocd

# Apply patches if needed (future-proofing)
echo "Configuring OpenOCD..."

# Bootstrap the build system
if [ -f "bootstrap" ]; then
    ./bootstrap
fi

# Configure with ESP support
./configure \
    --prefix="$INSTALL_PREFIX" \
    --enable-usb_blaster_libftdi \
    --enable-esp-usb-jtag \
    --disable-docs

echo "Building (this may take a few minutes)..."
make -j$(sysctl -n hw.ncpu)

echo "Installing..."
make install

# Add to PATH for current session
export PATH="$INSTALL_PREFIX/bin:$PATH"

echo ""
echo "OpenOCD 1.0 built successfully!"
echo ""
echo "To use it permanently, add this to your ~/.zshrc or ~/.bash_profile:"
echo "  export PATH=\"$INSTALL_PREFIX/bin:\\\$PATH\""
echo ""
echo "Verify installation:"
$INSTALL_PREFIX/bin/openocd --version

#!/bin/bash
set -e

APP_NAME="Lightspeed"
APP_DIR="$APP_NAME.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"

echo "Building $APP_NAME..."
cargo build --release

echo "Creating bundle structure..."
rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR"
mkdir -p "$RESOURCES_DIR"

echo "Copying executable..."
cp target/release/lightspeed "$MACOS_DIR/"

echo "Copying Info.plist..."
cp Info.plist "$CONTENTS_DIR/"

echo "Processing Icon..."
if [ -f "generated_icon.png" ]; then
    echo "Creating AppIcon.icns from generated_icon.png..."
    mkdir -p "AppIcon.iconset"
    
    # Generate different sizes
    sips -z 16 16     generated_icon.png --out "AppIcon.iconset/icon_16x16.png" > /dev/null
    sips -z 32 32     generated_icon.png --out "AppIcon.iconset/icon_16x16@2x.png" > /dev/null
    sips -z 32 32     generated_icon.png --out "AppIcon.iconset/icon_32x32.png" > /dev/null
    sips -z 64 64     generated_icon.png --out "AppIcon.iconset/icon_32x32@2x.png" > /dev/null
    sips -z 128 128   generated_icon.png --out "AppIcon.iconset/icon_128x128.png" > /dev/null
    sips -z 256 256   generated_icon.png --out "AppIcon.iconset/icon_128x128@2x.png" > /dev/null
    sips -z 256 256   generated_icon.png --out "AppIcon.iconset/icon_256x256.png" > /dev/null
    sips -z 512 512   generated_icon.png --out "AppIcon.iconset/icon_256x256@2x.png" > /dev/null
    sips -z 512 512   generated_icon.png --out "AppIcon.iconset/icon_512x512.png" > /dev/null
    sips -z 1024 1024 generated_icon.png --out "AppIcon.iconset/icon_512x512@2x.png" > /dev/null

    iconutil -c icns AppIcon.iconset
    cp AppIcon.icns "$RESOURCES_DIR/"
    rm -rf AppIcon.iconset
    echo "Icon packaged."
else
    echo "WARNING: generated_icon.png not found. App will have generic icon."
fi

echo "Done! $APP_DIR created."

# Touch important files to update modification times
touch "$RESOURCES_DIR/AppIcon.icns"
touch "$CONTENTS_DIR/Info.plist"
touch "$APP_DIR"

# Clear macOS icon cache
echo "Clearing macOS icon cache..."
killall Finder
killall Dock
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -kill -r -domain local -domain system -domain user
echo "Icon cache cleared. Icon should update immediately."

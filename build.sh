#!/bin/bash

# Build the Swift package in release mode
swift build -c release

# Create the App Bundle directories
mkdir -p FileID.app/Contents/MacOS
mkdir -p FileID.app/Contents/Resources

# Copy the executable into the bundle
cp .build/release/FileID FileID.app/Contents/MacOS/

# Copy Resources
cp Resources/* FileID.app/Contents/Resources/ || true

# Copy the Info.plist into the bundle
cp Info.plist FileID.app/Contents/

# Create a minimal PkgInfo file
echo "APPL????" > FileID.app/Contents/PkgInfo

# Ad-hoc sign the application bundle so macOS Gatekeeper allows it to ask for folder permissions
xattr -rc FileID.app
codesign --force --deep --entitlements FileID.entitlements -s - FileID.app

echo "FileID.app built successfully!"

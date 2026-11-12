#!/usr/bin/env bash
set -euo pipefail

echo "Configuring Waydroid runner..."

if ! command -v waydroid >/dev/null 2>&1; then
    if command -v pacman >/dev/null 2>&1; then
        pacman -S --noconfirm --needed waydroid dnsmasq lxc python-gbinder libgbinder libglibutil
    else
        echo "ERROR: waydroid binary not found. Install waydroid via your distribution package manager." >&2
        exit 1
    fi
fi

echo "Initializing Waydroid container..."
waydroid init -f

echo "Reloading D-Bus and starting waydroid-container.service..."
systemctl reload dbus 2>/dev/null || true
systemctl enable waydroid-container
systemctl restart waydroid-container

echo "Waydroid runner configuration complete."

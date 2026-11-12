#!/usr/bin/env bash
set -euo pipefail

NDK_URL="https://github.com/supremegamers/vendor_google_proprietary_ndk_translation-prebuilt/archive/68734c52556d3d7a6db34c603dd9276915c29f2f.zip"
NDK_COMMIT="68734c52556d3d7a6db34c603dd9276915c29f2f"
NDK_MD5="0b2207c490fcb400aa5c87fcf0d52d38"
RESETPROP_URL="https://raw.githubusercontent.com/casualsnek/waydroid_script/main/bin/x86_64/resetprop"

if [ -n "${RUSTYDROID_CACHE_DIR:-}" ]; then
    CACHE_DIR="$RUSTYDROID_CACHE_DIR"
elif [ -n "${SUDO_USER:-}" ]; then
    USER_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
    CACHE_DIR="${USER_HOME:-/tmp}/.cache/rustydroid"
else
    CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/rustydroid"
fi

mkdir -p "$CACHE_DIR"
ZIP_PATH="$CACHE_DIR/ndk_translation_a13.zip"
EXTRACT_DIR="$CACHE_DIR/ndk_extract"
RESETPROP_BIN="$CACHE_DIR/resetprop"

need_download=1
if [ -f "$ZIP_PATH" ]; then
    actual_md5="$(md5sum "$ZIP_PATH" | awk '{print $1}')"
    if [ "$actual_md5" = "$NDK_MD5" ]; then
        need_download=0
    fi
fi

if [ "$need_download" -eq 1 ]; then
    echo "Downloading Android 13 libndk_translation..."
    curl -fL -o "$ZIP_PATH" "$NDK_URL"
    actual_md5="$(md5sum "$ZIP_PATH" | awk '{print $1}')"
    if [ "$actual_md5" != "$NDK_MD5" ]; then
        echo "ERROR: libndk_translation md5 mismatch: got $actual_md5, expected $NDK_MD5" >&2
        exit 1
    fi
fi

if [ ! -f "$RESETPROP_BIN" ]; then
    echo "Downloading resetprop helper..."
    curl -fL -o "$RESETPROP_BIN" "$RESETPROP_URL"
fi

rm -rf "$EXTRACT_DIR"
unzip -q "$ZIP_PATH" -d "$EXTRACT_DIR"

SRC_PREBUILTS="$EXTRACT_DIR/vendor_google_proprietary_ndk_translation-prebuilt-$NDK_COMMIT/prebuilts"
OVERLAY_SYS="/var/lib/waydroid/overlay/system"
CFG_FILE="/var/lib/waydroid/waydroid.cfg"

if [ ! -d "$SRC_PREBUILTS" ]; then
    echo "ERROR: prebuilts directory missing in extracted archive" >&2
    exit 1
fi

rm -rf "$OVERLAY_SYS"
mkdir -p "$OVERLAY_SYS/etc/init"
cp -a "$SRC_PREBUILTS"/. "$OVERLAY_SYS"/

install -D -m 0755 -o root -g root "$RESETPROP_BIN" "$OVERLAY_SYS/etc/resetprop"

cat << 'SH' > "$OVERLAY_SYS/etc/resetprop.sh"
#!/system/bin/sh
while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
        ""|\#*) continue ;;
        *=*)
            key="${line%%=*}"
            val="${line#*=}"
            case "$key" in
                ro.*|dalvik.*|persist.*|sys.*)
                    /system/etc/resetprop "$key" "$val"
                    ;;
            esac
            ;;
    esac
done < /vendor/waydroid.prop

if [ -d /proc/sys/fs/binfmt_misc ] && [ -w /proc/sys/fs/binfmt_misc/register ]; then
    for entry in arm_exe arm_dyn arm64_exe arm64_dyn; do
        if [ ! -e "/proc/sys/fs/binfmt_misc/$entry" ] && [ -f "/system/etc/binfmt_misc/$entry" ]; then
            cat "/system/etc/binfmt_misc/$entry" > /proc/sys/fs/binfmt_misc/register
        fi
    done
fi
SH

cat << 'RC' > "$OVERLAY_SYS/etc/init/resetprop.rc"
on post-fs-data
    exec u:r:su:s0 root root -- /system/bin/sh /system/etc/resetprop.sh
RC

find "$OVERLAY_SYS" -type d -exec chmod 0755 {} +
find "$OVERLAY_SYS" -type f -exec chmod 0644 {} +
chown -R root:root "$OVERLAY_SYS"

chown -R root:2000 "$OVERLAY_SYS/bin"
find "$OVERLAY_SYS/bin" -type d -exec chmod 0755 {} +
find "$OVERLAY_SYS/bin" -type f -exec chmod 0755 {} +
chmod 0755 "$OVERLAY_SYS/etc/resetprop" "$OVERLAY_SYS/etc/resetprop.sh"

python3 - << 'PY'
import configparser

cfg_path = "/var/lib/waydroid/waydroid.cfg"
cfg = configparser.ConfigParser()
cfg.optionxform = str
cfg.read(cfg_path)
if "properties" not in cfg:
    cfg["properties"] = {}

ndk_props = {
    "ro.product.cpu.abilist": "x86_64,x86,arm64-v8a,armeabi-v7a,armeabi",
    "ro.product.cpu.abilist32": "x86,armeabi-v7a,armeabi",
    "ro.product.cpu.abilist64": "x86_64,arm64-v8a",
    "ro.system.product.cpu.abilist": "x86_64,x86,arm64-v8a,armeabi-v7a,armeabi",
    "ro.system.product.cpu.abilist32": "x86,armeabi-v7a,armeabi",
    "ro.system.product.cpu.abilist64": "x86_64,arm64-v8a",
    "ro.vendor.product.cpu.abilist": "x86_64,x86,arm64-v8a,armeabi-v7a,armeabi",
    "ro.vendor.product.cpu.abilist32": "x86,armeabi-v7a,armeabi",
    "ro.vendor.product.cpu.abilist64": "x86_64,arm64-v8a",
    "ro.dalvik.vm.native.bridge": "libndk_translation.so",
    "ro.enable.native.bridge.exec": "1",
    "ro.vendor.enable.native.bridge.exec": "1",
    "ro.vendor.enable.native.bridge.exec64": "1",
    "ro.ndk_translation.version": "0.2.3",
    "ro.dalvik.vm.isa.arm": "x86",
    "ro.dalvik.vm.isa.arm64": "x86_64",
}

for k, v in ndk_props.items():
    cfg["properties"][k] = v

with open(cfg_path, "w") as f:
    cfg.write(f)
PY

if [ -f "$CFG_FILE" ] && [ -f /var/lib/waydroid/waydroid_base.prop ]; then
    waydroid upgrade -o
fi

echo "ARM/ARM64 libndk_translation setup completed."

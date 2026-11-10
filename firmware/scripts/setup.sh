#!/bin/bash
# Sets up what building g1v0 on this Mac needs, in ~/ncs (see common.sh): NCS 2.5.1 with its Python environment,
# Zephyr SDK 0.16.5-1, and Even's 1.6.6 firmware, parts of which g1v0 embeds. This is what scouter_kit's
# scripts/setup.sh build does in Linux containers. Whatever is there already is left alone, so it's safe to run
# again. Needs uv, cmake, ninja and dtc, e.g. from Homebrew.
set -euo pipefail
. "$(dirname "$0")/common.sh"
mkdir -p "$NCS_ROOT"

if [ ! -x "$NCS_WORKSPACE/.venv/bin/python" ]; then
    echo "== Python environment"
    uv venv --python 3.11 "$NCS_WORKSPACE/.venv"
    uv pip install --python "$NCS_WORKSPACE/.venv/bin/python" -r "$FIRMWARE/scripts/requirements.lock"
fi

if [ ! -d "$NCS_WORKSPACE/zephyr" ]; then
    echo "== NCS 2.5.1 (about 5 GB)"
    cd "$NCS_WORKSPACE"
    [ -d .west ] || .venv/bin/west init -m https://github.com/nrfconnect/sdk-nrf --mr v2.5.1 .
    .venv/bin/west update
fi

# The commits g1v0 was built and tested with (scouter_kit's scripts/ncs_fetch.sh).
cd "$NCS_WORKSPACE"
while read -r sha dir; do
    [ "$(git -C "$dir" rev-parse HEAD)" = "$sha" ] || { echo "NCS: $dir is not at $sha"; exit 1; }
done <<EOF
87355af5914e498f449b7a68bb5641031a7b8885 nrf
83980fe1679441be9b0e1db556a353f6118fe14f zephyr
edfe1e1465dbc698bf9a195816247913490ab391 bootloader/mcuboot
acea48fc8a5eb227033b55e6ec012731218e257f modules/crypto/mbedtls
3e9a49d2672ec01435ffbf0d788db6d95ef28de0 modules/crypto/tinycrypt
9784731461018d3e983604698fbbed6af2bea801 modules/hal/nordic
67fd8bb88d3136738661fa8bb5f9989103f4599e modules/lib/zcbor
ab72f33c86db7252dbf9a3ffec86c6b7fc6a9da7 nrfxlib
EOF

if [ ! -x "$ZEPHYR_SDK/arm-zephyr-eabi/bin/arm-zephyr-eabi-gcc" ]; then
    echo "== Zephyr SDK 0.16.5-1"
    host=macos-$(uname -m | sed 's/arm64/aarch64/')
    url=https://github.com/zephyrproject-rtos/sdk-ng/releases/download/v0.16.5-1
    dl=$(mktemp -d)
    cd "$dl"
    curl -fsSLO "$url/sha256.sum"
    for f in "zephyr-sdk-0.16.5-1_${host}_minimal.tar.xz" "toolchain_${host}_arm-zephyr-eabi.tar.xz"; do
        curl -fsSLO "$url/$f"
        grep " $f\$" sha256.sum | shasum -a 256 -c -
    done
    tar xf "zephyr-sdk-0.16.5-1_${host}_minimal.tar.xz" -C "$NCS_ROOT"
    tar xf "toolchain_${host}_arm-zephyr-eabi.tar.xz" -C "$ZEPHYR_SDK"
    rm -rf "$dl"
fi

# Even's 1.6.6 from Even's own server, checked against scouter_kit's stock_images.txt. g1v0 embeds its net core
# image (reflashed only if Bluetooth never comes up) and the TouchBar controller's configuration table.
STOCK=$G1_WORK/firmware/1.6.6
if [ ! -f "$G1_WORK/fw/1.6.6_aw9320x_table.bin" ]; then
    echo "== Even's firmware 1.6.6"
    mkdir -p "$STOCK" "$G1_WORK/fw"
    sums=$G1V0/stock_images.txt
    url=$(awk '$1 == "1.6.6" && $2 == "zip" {print $4}' "$sums")
    zip=$STOCK/$(basename "$url")
    curl -fsSL --proto '=https' -o "$zip" "$url"
    check() { echo "$(awk -v f="$1" '$1 == "1.6.6" && $2 == f {print $3}' "$sums")  $2" | shasum -a 256 -c -; }
    check zip "$zip"
    unzip -o -q "$zip" -d "$STOCK"
    for f in app_update.bin net_core_app_update.bin manifest.json; do check "$f" "$STOCK/$f"; done
    tail -c +513 "$STOCK/net_core_app_update.bin" > "$G1_WORK/fw/1.6.6_net_raw.bin"
    dd if="$STOCK/app_update.bin" of="$G1_WORK/fw/1.6.6_aw9320x_table.bin" bs=1 skip=$((0x81b58)) count=352 2>/dev/null
    check aw9320x_table "$G1_WORK/fw/1.6.6_aw9320x_table.bin"
fi

echo "== ready in $NCS_ROOT"

#!/bin/bash
# Builds g1v0 for the G1 as version VERSION (default 0.5.60), the way scouter_kit's scripts/build.sh builds its
# install image, with our patches (../patches) applied to a copy of scouter_kit at the submodule's commit
# (~/ncs/g1work/src), so the submodule stays as published. The build number is the count of patches, so 0.5.60 with
# one patch is 0.5.60+1. --upstream builds without them. The image to install is
# ~/ncs/g1work/build/g1v0-v<version>/zephyr/app_update.bin.
set -euo pipefail
. "$(dirname "$0")/common.sh"
patches=("$FIRMWARE"/patches/*.patch)
if [ "${1:-}" = --upstream ]; then
    patches=()
    shift
fi
version=${1:-0.5.60}+${#patches[@]}
out=$G1_WORK/build/g1v0-v${version//./}

src=$G1_WORK/src
commit=$(git -C "$FIRMWARE/scouter_kit" rev-parse HEAD)
[ -e "$src/.git" ] || git -C "$FIRMWARE/scouter_kit" worktree add -q --detach "$src" "$commit"
git -C "$src" checkout -q --detach -f "$commit"
git -C "$src" clean -q -fdx
for p in ${patches[@]+"${patches[@]}"}; do
    git -C "$src" apply "$p"
    echo "applied $(basename "$p")"
done

. "$NCS_WORKSPACE/.venv/bin/activate"
export PATH="/opt/homebrew/bin:$PATH"
export ZEPHYR_BASE=$NCS_WORKSPACE/zephyr
export ZEPHYR_TOOLCHAIN_VARIANT=zephyr
export ZEPHYR_SDK_INSTALL_DIR=$ZEPHYR_SDK
# CMake 4 refuses the cmake_minimum_required versions below 3.5 that some NCS 2.5.1 modules still ask for.
export CMAKE_POLICY_VERSION_MINIMUM=3.5
export CCACHE_DISABLE=1
cd "$NCS_WORKSPACE"
west build -p always -b nrf5340dk_nrf5340_cpuapp -d "$out" "$src/g1_firmware/firmware" -- \
    "-DCONFIG_MCUBOOT_IMGTOOL_SIGN_VERSION=\"$version\""
echo "image: $out/zephyr/app_update.bin"

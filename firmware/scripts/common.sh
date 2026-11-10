# Where the scripts here keep things. Sourced, not run. NCS_ROOT overrides the default of ~/ncs, where Nordic's own
# tools put NCS.
NCS_ROOT=${NCS_ROOT:-$HOME/ncs}
NCS_WORKSPACE=$NCS_ROOT/v2.5.1
ZEPHYR_SDK=$NCS_ROOT/zephyr-sdk-0.16.5-1
# g1v0's work directory: Even's images the build embeds parts of, and the builds.
export G1_WORK=${G1_WORK:-$NCS_ROOT/g1work}
FIRMWARE=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
G1V0=$FIRMWARE/scouter_kit/g1_firmware

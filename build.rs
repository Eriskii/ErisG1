// Builds scouter_kit's VM core (MIT, the firmware/scouter_kit submodule) with the simulated arm around it, for
// `erisg1 v0 preview` and the tests.
fn main() {
    let vm = "firmware/scouter_kit/src/vm";
    for file in ["vm.c", "vm_gfx.c", "vm.h", "vm_gfx.h"] {
        println!("cargo:rerun-if-changed={vm}/{file}");
    }
    println!("cargo:rerun-if-changed=src/apps/sim.c");
    cc::Build::new()
        .file(format!("{vm}/vm.c"))
        .file(format!("{vm}/vm_gfx.c"))
        .file("src/apps/sim.c")
        .include(vm)
        .std("c11")
        .opt_level(2)
        .warnings(false)
        .compile("g1vm");
}

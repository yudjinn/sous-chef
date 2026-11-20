# lint-precision: trimmed from OE-core (scarthgap) recipes-devtools/rust/rust-llvm_1.75.0.bb:31-34
# and meta-oe (scarthgap) recipes-core/packagegroups/packagegroup-meta-oe.bb:327.
# CFLAGS comes from configuration: the recipe cannot leave "-g" out of it.
CFLAGS:remove = "-g"
# RDEPENDS of the packagegroup is set in this file: leave the item out there.
RDEPENDS:packagegroup-meta-oe-devtools = "breakpad lshw ply"
RDEPENDS:packagegroup-meta-oe-devtools:remove:powerpc64 = "breakpad lshw ply"

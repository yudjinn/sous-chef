# lint-precision: trimmed from meta-oe (kirkstone) recipes-support/colord/colord-native.bb:1-3.
# PV comes from colord.inc, not from the file name.
require ${BPN}.inc

FILESEXTRAPATHS:prepend := "${THISDIR}/${BPN}-native:"

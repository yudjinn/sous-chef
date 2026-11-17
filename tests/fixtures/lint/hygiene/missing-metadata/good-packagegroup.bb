# Shaped after OE-core's packagegroup-core-boot.bb: SUMMARY, no HOMEPAGE, LICENSE from
# packagegroup.bbclass.
SUMMARY = "Minimal boot requirements"

inherit packagegroup

RDEPENDS:${PN} = "base-files base-passwd"

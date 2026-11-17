# The idiom, and its variants.
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"
FILESEXTRAPATHS:prepend:qemux86 := "${THISDIR}/qemu:"
FILESEXTRAPATHS:append := ":${THISDIR}/late"
FILESEXTRAPATHS:prepend = "${COREBASE}/shared-files:"
FILESEXTRAPATHS_prepend := "legacy syntax is another rule's business"

# The idiom, and its variants.
FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"
FILESEXTRAPATHS:prepend:qemux86 := "${THISDIR}/qemu:"
FILESEXTRAPATHS:append := ":${THISDIR}/late"
FILESEXTRAPATHS:prepend = "${COREBASE}/shared-files:"
FILESEXTRAPATHS_prepend := "legacy syntax is another rule's business"
# meta-arm's linux-yocto%.bbappend:1-3: the value is a variable this file sets with the colon.
ARMFILESPATHS := "${THISDIR}/files:"
FILESEXTRAPATHS:prepend:qemuarm64 = "${ARMFILESPATHS}"
# meta-arm's arm-ffa-transport.inc:1: a variable set elsewhere cannot be judged.
FILESEXTRAPATHS:prepend := "${ARMFILESPATHS_ELSEWHERE}"
FILESEXTRAPATHS:append := "${ARM_APPEND_ELSEWHERE}"
FILESEXTRAPATHS:prepend := "${@d.getVar('X') or ''}"

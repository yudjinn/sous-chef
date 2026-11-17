require hello.inc

LIC_FILES_CHKSUM = "file://COPYING;md5=0835ade698e0bcf8506ecda2f7b4f302"

SRC_URI += "file://hello.patch"
SRC_URI[sha256sum] = "2222222222222222222222222222222222222222222222222222222222222222"

PACKAGECONFIG ??= "greeting"
PACKAGECONFIG[greeting] = "--enable-greeting,--disable-greeting"

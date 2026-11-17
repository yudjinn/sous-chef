# Shaped after meta-kiss's u-boot-kiss_git.bb: rewrites what the shared .inc set.
SUMMARY = "U-Boot from mainline"
require shared.inc

SRC_URI = "git://example.org/u-boot.git;protocol=https;branch=master"
SRCREV = "88dc2788777babfd6322fa655df549a019aa1e69"
LIC_FILES_CHKSUM = "file://Licenses/README;md5=2ca5f2c35c8cc335f0a19756634782f1"

SUMMARY = "Routine QA skips"
LICENSE = "MIT"
INSANE_SKIP:${PN} += "dev-so arch already-stripped"
INSANE_SKIP:${PN}-dev += "dev-elf"

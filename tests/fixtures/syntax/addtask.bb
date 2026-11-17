addtask do_deploy after do_install before do_build
addtask deploy_extra
addtask package_qa before do_build after do_package do_packagedata
addtask do_check before do_build before do_image after do_compile
addtask do_after_only after do_compile
deltask do_configure
deltask package_write_rpm

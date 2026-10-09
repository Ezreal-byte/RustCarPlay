# SPDX-License-Identifier: GPL-3.0-only
"""Read-only Windows driver-policy checks; never registers or installs anything.

Pass a built helper and the verified libusb 1.4.0.2 CAT/SYS fixture. Only temporary
copies are altered. No driver, catalog database or certificate store is changed.
"""

import argparse
import pathlib
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", type=pathlib.Path, required=True)
    parser.add_argument("--catalog", type=pathlib.Path, required=True)
    parser.add_argument("--driver", type=pathlib.Path, required=True)
    args = parser.parse_args()
    helper = args.helper.resolve(strict=True)
    catalog = args.catalog.resolve(strict=True)
    driver = args.driver.resolve(strict=True)

    def check(label, success, *arguments):
        result = subprocess.run(
            [str(helper), *map(str, arguments)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=60,
            check=False,
        )
        if (result.returncode == 0) != success:
            raise AssertionError(f"{label}: unexpected exit {result.returncode}: {result.stderr[:400]}")
        print(f"PASS {label}")

    check("pinned catalog member", True, "--catalog", catalog, driver)
    check("standalone WHCP catalog", True, "--embedded", catalog)
    check("unsigned executable is not a trusted driver", False, "--embedded", helper)
    check("unrelated file is not a catalog member", False, "--catalog", catalog, helper)
    with tempfile.TemporaryDirectory(prefix="rustcarplay-driver-verification-") as temporary:
        directory = pathlib.Path(temporary)
        bad_driver = directory / "tampered.sys"
        bad_catalog = directory / "tampered.cat"
        shutil.copyfile(driver, bad_driver)
        shutil.copyfile(catalog, bad_catalog)
        check("renamed intact copies", True, "--catalog", bad_catalog, bad_driver)
        with bad_driver.open("r+b") as stream:
            stream.seek(1024)
            value = stream.read(1)
            assert value, "driver fixture is unexpectedly short"
            stream.seek(1024)
            stream.write(bytes([value[0] ^ 1]))
        check("modified driver rejected", False, "--catalog", catalog, bad_driver)
        with bad_catalog.open("r+b") as stream:
            stream.seek(32)
            value = stream.read(1)
            assert value, "catalog fixture is unexpectedly short"
            stream.seek(32)
            stream.write(bytes([value[0] ^ 1]))
        check("modified catalog rejected", False, "--catalog", bad_catalog, driver)


if __name__ == "__main__":
    main()

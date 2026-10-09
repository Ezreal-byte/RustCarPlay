"""Create MSVC import libraries from verified official runtime DLL exports.

Rust GStreamer bindings are pregenerated and do not require C headers. This
workspace-local helper requires MSVC lib.exe; it does not modify the system.
"""
import ctypes
import os
from pathlib import Path
import struct
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
prefix = root / '.local' / 'gstreamer'
libdir = prefix / 'lib'
pcdir = libdir / 'pkgconfig'
pcdir.mkdir(parents=True, exist_ok=True)
vswhere = Path(os.environ['ProgramFiles(x86)']) / 'Microsoft Visual Studio/Installer/vswhere.exe'
libexe = subprocess.check_output([str(vswhere), '-latest', '-products', '*', '-find',
                                 r'VC\Tools\MSVC\**\bin\Hostx64\x64\lib.exe'], text=True).strip().splitlines()[0]

def exports(path):
    blob = path.read_bytes()
    u16 = lambda n: struct.unpack_from('<H', blob, n)[0]
    u32 = lambda n: struct.unpack_from('<I', blob, n)[0]
    pe = u32(0x3c)
    assert blob[pe:pe+4] == b'PE\0\0' and u16(pe + 24) == 0x20b
    optional = pe + 24
    section_start = optional + u16(pe + 20)
    sections = []
    for i in range(u16(pe+6)):
        pos = section_start + i * 40
        sections.append((u32(pos+12), max(u32(pos+8), u32(pos+16)), u32(pos+20), u32(pos+36)))
    def locate(rva):
        for va, size, raw, flags in sections:
            if va <= rva < va + size:
                return raw + rva - va, flags
        raise ValueError('Unmapped export RVA')
    export_rva = u32(optional + 112)
    export_size = u32(optional + 116)
    directory = locate(export_rva)[0]
    functions = locate(u32(directory+28))[0]
    names = locate(u32(directory+32))[0]
    ordinals = locate(u32(directory+36))[0]
    result = []
    for i in range(u32(directory+24)):
        name_at = locate(u32(names+4*i))[0]
        name = blob[name_at:blob.index(b'\0', name_at)].decode('ascii')
        address = u32(functions+4*u16(ordinals+2*i))
        if export_rva <= address < export_rva+export_size:
            raise ValueError('Forwarded exports require explicit handling')
        is_code = locate(address)[1] & 0x20
        result.append(name + ('' if is_code else ' DATA'))
    return result

dll_search = os.add_dll_directory(str(prefix / 'bin'))
glib = ctypes.CDLL(str(prefix / 'bin' / 'glib-2.0-0.dll'))
glib_version = '.'.join(str(ctypes.c_uint.in_dll(glib, 'glib_' + field + '_version').value)
                        for field in ('major','minor','micro'))
gst = ctypes.CDLL(str(prefix / 'bin' / 'gstreamer-1.0-0.dll'))
parts = [ctypes.c_uint() for _ in range(4)]
gst.gst_version(*(ctypes.byref(part) for part in parts))
gst_version = '.'.join(str(part.value) for part in parts[:3])
packages = {
    'glib-2.0': ('glib-2.0-0', glib_version, ''),
    'gobject-2.0': ('gobject-2.0-0', glib_version, 'glib-2.0'),
    'gio-2.0': ('gio-2.0-0', glib_version, 'gobject-2.0'),
    'gstreamer-1.0': ('gstreamer-1.0-0', gst_version, 'gobject-2.0'),
    'gstreamer-base-1.0': ('gstbase-1.0-0', gst_version, 'gstreamer-1.0'),
    'gstreamer-app-1.0': ('gstapp-1.0-0', gst_version, 'gstreamer-base-1.0'),
    'gstreamer-video-1.0': ('gstvideo-1.0-0', gst_version, 'gstreamer-base-1.0'),
    'gstreamer-audio-1.0': ('gstaudio-1.0-0', gst_version, 'gstreamer-base-1.0'),
}
for package, (dllname, version, requires) in packages.items():
    dll = prefix / 'bin' / (dllname + '.dll')
    definition = libdir / (package + '.def')
    definition.write_text('LIBRARY ' + dll.name + '\nEXPORTS\n' + '\n'.join(exports(dll)) + '\n', encoding='ascii')
    subprocess.run([libexe, '/nologo', '/machine:x64', '/def:' + str(definition),
                    '/out:' + str(libdir / (package + '.lib'))], check=True, stdout=subprocess.DEVNULL)
    (pcdir / (package + '.pc')).write_text(
        f'prefix={prefix.as_posix()}\nlibdir=${{prefix}}/lib\nName: {package}\n'
        f'Description: Official GStreamer runtime import library\nVersion: {version}\n'
        f'Requires: {requires}\nLibs: -L${{libdir}} -l{package}\nCflags:\n', encoding='utf-8')
print('Prepared GStreamer', gst_version, 'and GLib', glib_version, 'import libraries')

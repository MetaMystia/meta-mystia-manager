#!/usr/bin/env python3
"""检查 PE 产物的导入表是否还依赖 Win8+ / Win10+ 的 API。

Windows 7 只保证到 Win7 时代的 API：如果 exe 静态导入了 WaitOnAddress、ProcessPrng
这类新系统的函数，加载器会在启动阶段直接失败。这里把导入表打出来并做断言，
供 CI 在发版前拦截（只有 Windows 宿主 + YY-Thunks 的构建能通过）。

用法：python check_win7_imports.py <exe> [<exe> ...]
"""

import struct
import sys

# Windows 的 CI 控制台默认是 cp1252，直接打印中文会 UnicodeEncodeError
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
if hasattr(sys.stderr, "reconfigure"):
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")

IMAGE_ORDINAL_FLAG64 = 0x8000000000000000
IMAGE_ORDINAL_FLAG32 = 0x80000000

DIRECTORY_IMPORT = 1
DIRECTORY_DELAY_IMPORT = 13

# Win8+ / Win10+ 才有的函数，出现即视为产物无法在 Win7 上启动
FORBIDDEN_FUNCTIONS = {
    "WaitOnAddress",
    "WakeByAddressAll",
    "WakeByAddressSingle",
    "ProcessPrng",
    "GetSystemTimePreciseAsFileTime",
    "SetThreadDescription",
    "GetTempPath2W",
    "CreateFile2",
    "GetPackageFullName",
    "DiscardVirtualMemory",
}

# 依赖这些运行时意味着需要额外的运行库或 Win8+ 系统组件
FORBIDDEN_DLL_PREFIXES = (
    "api-ms-win-crt-",
    "vcruntime",
    "msvcp",
    "api-ms-win-core-synch-l1-2-0",
    "bcryptprimitives",
)


def read_sections(data, pe_offset):
    (_, section_count, _, _, _, optional_size, _) = struct.unpack_from(
        "<HHIIIHH", data, pe_offset + 4
    )
    optional = pe_offset + 24
    magic = struct.unpack_from("<H", data, optional)[0]
    is_pe32_plus = magic == 0x20B
    directories = optional + (112 if is_pe32_plus else 96)

    def directory(index):
        offset = directories + index * 8
        if optional + optional_size < offset + 8:
            return (0, 0)

        return struct.unpack_from("<II", data, offset)

    sections = []
    first_section = optional + optional_size

    for index in range(section_count):
        offset = first_section + index * 40
        name = data[offset : offset + 8].rstrip(b"\0").decode(errors="replace")
        virtual_size, virtual_address, raw_size, raw_pointer = struct.unpack_from(
            "<IIII", data, offset + 8
        )
        sections.append((virtual_address, virtual_size, raw_pointer, raw_size, name))

    return is_pe32_plus, directory, sections


def rva_to_offset(sections, rva):
    for virtual_address, virtual_size, raw_pointer, raw_size, _ in sections:
        if virtual_address <= rva < virtual_address + max(virtual_size, raw_size):
            return raw_pointer + (rva - virtual_address)

    return None


def read_c_string(data, offset):
    end = data.index(b"\0", offset)

    return data[offset:end].decode(errors="replace")


def read_imports(data, table_offset, is_pe32_plus, rva_to_off):
    """读取导入描述符表；返回 (dll, 函数名) 列表"""
    imports = []
    step = 8 if is_pe32_plus else 4
    ordinal_flag = IMAGE_ORDINAL_FLAG64 if is_pe32_plus else IMAGE_ORDINAL_FLAG32

    index = 0
    while True:
        descriptor = table_offset + index * 20
        # OriginalFirstThunk, TimeDateStamp, ForwarderChain, Name, FirstThunk
        original, _, _, name_rva, first_thunk = struct.unpack_from(
            "<IIIII", data, descriptor
        )
        index += 1

        if original == 0 and name_rva == 0 and first_thunk == 0:
            break

        name_offset = rva_to_off(name_rva)
        dll = read_c_string(data, name_offset) if name_offset is not None else "?"

        thunk_rva = original or first_thunk
        thunk_offset = rva_to_off(thunk_rva)
        if thunk_offset is None:
            continue

        entry = 0
        while True:
            value = struct.unpack_from("<Q" if is_pe32_plus else "<I", data, thunk_offset + entry * step)[0]
            if value == 0:
                break

            if value & ordinal_flag:
                imports.append((dll, f"#{value & 0xFFFF}"))
            else:
                import_offset = rva_to_off(value & 0x7FFFFFFF)
                if import_offset is not None:
                    # IMAGE_IMPORT_BY_NAME: 2 字节 hint + 名字
                    imports.append((dll, read_c_string(data, import_offset + 2)))

            entry += 1

    return imports


def collect_imports(path):
    data = open(path, "rb").read()
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise SystemExit(f"{path}: 不是有效的 PE 文件")

    is_pe32_plus, directory, sections = read_sections(data, pe_offset)
    imports = []

    for index, label in (
        (DIRECTORY_IMPORT, "import"),
        (DIRECTORY_DELAY_IMPORT, "delay import"),
    ):
        rva, size = directory(index)
        if rva == 0 or size == 0:
            continue

        offset = rva_to_offset(sections, rva)
        if offset is None:
            continue

        for dll, function in read_imports(
            data, offset, is_pe32_plus, lambda rva: rva_to_offset(sections, rva)
        ):
            imports.append((dll, function, label))

    return imports


def main(paths):
    failures = 0

    for path in paths:
        imports = collect_imports(path)
        dlls = sorted({dll for dll, _, _ in imports})

        print(f"=== {path} ===")
        print(f"DLL 依赖（{len(dlls)}）：{', '.join(dlls)}")

        violations = []

        for dll, function, table in imports:
            lowered = dll.lower()

            if any(lowered.startswith(prefix) for prefix in FORBIDDEN_DLL_PREFIXES):
                violations.append(f"{dll} :: {function}（{table}）")
            elif function in FORBIDDEN_FUNCTIONS:
                violations.append(f"{dll} :: {function}（{table}）")

        if violations:
            failures += 1
            print("发现 Win8+/Win10+ 依赖，产物无法在 Windows 7 上启动：")
            for item in violations:
                print(f"  - {item}")
        else:
            print("没有发现 Win8+/Win10+ 导入，Windows 7 可加载。")

        print()

    return 1 if failures else 0


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)

    sys.exit(main(sys.argv[1:]))

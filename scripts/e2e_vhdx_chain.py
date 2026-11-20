"""Reconstruct Windows differencing VHDX chains and recover actual NTFS unlinks."""
import argparse
from contextlib import ExitStack
import mmap
import json
from pathlib import Path
import shutil
import struct
import sys
import tempfile
import uuid

from e2e import check
from e2e_qcow2_backing import checked_import
from e2e_vhd_chain import compare_driver, independent_base, member
from e2e_vhdx import CORPUS, digest, native_import, recover, verify_guest

REF = Path(__file__).resolve().parents[1] / 'tests/vhdx-chain'
MIB = 1 << 20


def u32(b, at):
    return struct.unpack_from('<I', b, at)[0]


def u64(b, at):
    return struct.unpack_from('<Q', b, at)[0]


def guid(b, at):
    return str(uuid.UUID(bytes_le=b[at:at + 16]))


def tables(b):
    region = 3 * 65536
    regions = {guid(b, i): u64(b, i+16) for i in range(region+16, region+16+32*u32(b, region+8), 32)}
    meta = regions['8b7ca206-4790-4b9a-b8fe-575f050f886e']
    bat = regions['2dc27766-f623-4200-9d64-115e9bfd4a08']
    entries = {guid(b, i): (meta + u32(b, i+16), u32(b, i+20)) for i in
               range(meta+32, meta+32+32*int.from_bytes(b[meta+10:meta+12], 'little'), 32)}
    return bat, entries


def file_origins(raw, chain, hashes):
    # Independently walk the Windows BAT and sector bitmaps for each sector of
    # seven nonresident files. Check physical source bytes against each original.
    with ExitStack() as handles:
        views = []
        for path in chain:
            source = handles.enter_context(path.open('rb'))
            disk = handles.enter_context(mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_READ))
            bat, items = tables(disk)
            block = u32(disk, items['caa16737-fa36-4d43-b3b6-33f0aa44e76b'][0])
            sector = u32(disk, items['8141bf1d-a96f-4709-ba47-f233a8faab5f'][0])
            views.append((path.name, disk, bat, block, sector))
        src = handles.enter_context(raw.open('rb'))
        guest = handles.enter_context(mmap.mmap(src.fileno(), 0, access=mmap.ACCESS_READ))
        origins = {}
        for name in sorted(hashes):
            original = (CORPUS/name).read_bytes()
            at = guest.find(original)
            check(at >= 0 and guest.find(original, at + 1) == -1, name)
            counts = {}
            pos = 0
            while pos < len(original):
                off = at + pos
                for member_name, disk, bat, block, sector in views:
                    ratio = (1 << 23)*sector//block
                    index = off//block
                    entry = u64(disk, bat + 8*(index + index//ratio))
                    state = entry & 7
                    n = min(sector - off%sector, len(original) - pos)
                    if state == 0:
                        continue
                    if state == 7:
                        bitmap = u64(disk, bat + 8*((index//ratio+1)*(ratio+1)-1)) & ~(MIB-1)
                        bit = (off//sector) % (1 << 23)
                        if not disk[bitmap + bit//8] & (1 << (bit%8)):
                            continue
                    check(state in (2,6,7), (name, off, state))
                    physical = (entry & ~(MIB-1)) + off%block
                    data = bytes(n) if state == 2 else disk[physical:physical+n]
                    check(data == original[pos:pos+n], (name, off, member_name))
                    counts[member_name] = counts.get(member_name, 0) + n
                    break
                else:
                    raise AssertionError(('original bytes lack a recorded owner', name, off))
                pos += n
            origins[name] = counts
        return origins


def controls(binary, root, chain, spec):
    child, middle, base = chain
    altered = root / 'altered.vhdx'
    raw = root / 'rejected.raw'
    data = child.read_bytes()
    bat, items = tables(data)
    params = items['caa16737-fa36-4d43-b3b6-33f0aa44e76b'][0]
    logical = items['8141bf1d-a96f-4709-ba47-f233a8faab5f'][0]
    page = items['beca12ab-b2e6-4523-93ef-c309e000c746'][0]
    locator = items['a8d35f2d-b30b-454d-abf7-d3d84834ab0c'][0]
    chunk = (1 << 23) * u32(data, logical) // u32(data, params)
    bitmap_entry = bat + 8 * chunk
    partial = next(i for i in range(chunk) if u64(data, bat+8*i) & 7 == 7)
    payload = u64(data, bat + partial*8) & ~(MIB-1)
    bitmap = u64(data, bitmap_entry) & ~(MIB-1)
    check(payload and bitmap, 'actual partial BAT entry and independent bitmap are required')
    cases = []
    def reject(label, paths, options=(), code=4):
        checked_import(binary, paths, raw, options, code, 'vhdx')
        cases.append(label)

    reject('missing_chain', [child])
    reject('missing_base', [child, middle], code=2)
    reject('wrong_order', [child, base, middle])
    reject('extra_parent', [middle, base, child])
    reject('repeated_file', [child, middle, middle], code=2)
    shutil.copyfile(middle, altered)
    reject('wrong_basename', [child, altered, base])
    sibling = root / 'sibling'
    sibling.mkdir()
    wrong = sibling / middle.name
    shutil.copyfile(base, wrong)
    reject('same_basename_wrong_active_write_guid', [child, wrong])
    reject('wrong_logical_length', chain, ['--raw-bytes', spec['guest_bytes'] - 512])
    reject('wrong_logical_hash', chain, ['--raw-sha256', '11' * 32])
    u64le = lambda value: struct.pack('<Q', value)
    u32le = lambda value: struct.pack('<I', value)
    linkage = locator+u32(data, locator+24)
    changes = {
        'wrong_page83': (page, bytes(range(16))),
        'locator_type': (locator, bytes(16)),
        'locator_flags': (locator+16, b'\x01\x00'),
        'locator_count': (locator+18, b'\xff\xff'),
        'locator_overflow': (locator+20, u32le(0xfffffff0)),
        'locator_odd_utf16': (locator+28, b'\x03\x00'),
        'locator_overlaps_directory': (locator+20, bytes(4)),
        'locator_overlapping_values': (locator+36, data[locator+24:locator+28]),
        'locator_invalid_guid': (linkage, 'x'.encode('utf-16le')),
        'linkage_guid_mismatch': (linkage, ('{'+str(uuid.UUID(int=42))+'}').encode('utf-16le')),
        'locator_invalid_utf16': (linkage, b'\x00\xdc'),
        'has_parent_inconsistent': (params+4, bytes(4)),
        'missing_bitmap': (bitmap_entry, bytes(8)),
        'invalid_bitmap_state': (bitmap_entry, u64le(bitmap|7)),
        'bitmap_overlaps_payload': (bitmap_entry, u64le(payload|6)),
        'bitmap_overlaps_directory': (bitmap_entry, u64le(bat|6)),
        'bitmap_past_end': (bitmap_entry, u64le((1 << 48)|6)),
        'bitmap_reserved_bits': (bitmap_entry, u64le(bitmap|14)),
        'allocated_padding_slot': (bitmap_entry-8, u64le(payload|6)),
        'partial_payload_without_offset': (bat+partial*8, u64le(7)),
        'undefined_payload': (bat+partial*8, u64le(1)),
        'discarded_payload': (bat+partial*8, u64le(3)),
    }
    for label,(at,value) in changes.items():
        altered.write_bytes(data[:at]+value+data[at+len(value):])
        reject(label, [altered, middle, base])
    # Alter a present sector in a partially allocated payload without changing any
    # structural metadata. Its bytes must still be checked before publishing the RAW.
    sector_size = u32(data, logical)
    first_bit = partial * u32(data, params) // sector_size
    bit = next(i for i in range(u32(data, params) // sector_size)
               if data[bitmap+(first_bit+i)//8] & (1 << ((first_bit+i)%8)))
    at = payload + bit * sector_size
    altered.write_bytes(data[:at] + bytes([data[at]^1]) + data[at+1:])
    reject('changed_payload_known_hash', [altered, middle, base], ['--raw-sha256', spec['guest_sha256']])
    return sorted(cases)


def verify(binary, root, tsk=False, windows_driver=False):
    result = {}
    for case in ('4kn-child', '4kn-two-children', 'dynamic-child', 'dynamic-two-children', 'fixed-child'):
        ref = REF / case
        metadata = json.loads((ref/'manifest.json').read_text())
        check(metadata['relocated_driver_verified'], ref)
        folder = root / ref.name
        folder.mkdir()
        chain = []
        raw = folder / 'logical.raw'
        generations = []
        for spec in metadata['members']:
            path = folder/spec['filename']
            member(path, spec, ref)
            chain.insert(0, path)
            report = checked_import(binary, chain, raw, ['--raw-bytes', spec['guest_bytes'],
                        '--raw-sha256', spec['guest_sha256']], kind='vhdx')
            verify_guest(raw, report, spec)
            oracle = ref / (path.name + '.guest.raw.xz')
            check(digest(oracle) == spec['guest_xz_sha256'], oracle)
            compare_driver(raw, oracle)
            if windows_driver:
                sys.path.insert(0, str(REF.parent/'vhd-chain'))
                from capture_driver_raw import read_guest
                observed = folder/'current-driver.raw.xz'
                read_guest(path, spec, observed)
                compare_driver(raw, observed)
                observed.unlink()
            if tsk and len(chain) == 1:
                independent_base(path, raw, folder)
            generations.append(report['sha256'])
            if len(chain) < len(metadata['members']):
                raw.unlink()
        hashes = recover(binary, raw, folder/'ordinary', metadata, folder='deletions')
        check(recover(binary, raw, folder/'indexed', metadata, indexed=True, folder='deletions') == hashes, ref)
        proof = dict(originals=hashes, generations=generations)
        if ref.name in ('dynamic-two-children', '4kn-two-children'):
            proof['bytes_from_each_generation'] = file_origins(raw, chain, hashes)
        raw.unlink()
        if ref.name in ('dynamic-two-children', '4kn-two-children'):
            result[ref.name+'_refused'] = controls(binary, folder, chain, metadata['members'][-1])
        auto = native_import(binary, chain[0], raw, segments=chain[1:])
        verify_guest(raw, auto, metadata['members'][-1])
        check(all(digest(folder/s['filename']) == s['sha256'] for s in metadata['members']), 'source changed')
        result[ref.name] = proof
        print(ref.name, len(chain), 'Windows VHDX generations and seven real deletions match all bytes and SHA-256', flush=True)
        shutil.rmtree(folder)
    check(sum('generations' in v for v in result.values() if isinstance(v, dict)) == 5, result.keys())
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--tsk', action='store_true')
    parser.add_argument('--windows-driver', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as folder:
        proof = verify(args.binary.resolve(), Path(folder), args.tsk, args.windows_driver)
    if args.output:
        args.output.write_text(json.dumps(proof, indent=2, sort_keys=True)+'\n', encoding='utf-8')

#!/usr/bin/env python3
"""Writes test-city.mmdb: a MaxMind DB of four networks in the documentation ranges, in the shape
of DB-IP's "IP to City Lite" (city, subdivisions, country, continent, location). It is IPv6, with
the IPv4 networks under ::/96 as the real database has them. Standard library only.

Usage: python3 make_mmdb.py [out file]   (default: test-city.mmdb beside this script)
"""
import ipaddress
import os
import struct
import sys

NETWORKS = [
    ("203.0.113.0/24", {
        "city": {"names": {"en": "Paris"}},
        "continent": {"code": "EU", "names": {"en": "Europe"}},
        "country": {"iso_code": "FR", "names": {"en": "France"}},
        "location": {"latitude": 48.8566, "longitude": 2.3522},
        "subdivisions": [{"names": {"en": "Île-de-France"}}],
    }),
    ("198.51.100.0/24", {
        "city": {"names": {"en": "Toronto"}},
        "continent": {"code": "NA", "names": {"en": "North America"}},
        "country": {"iso_code": "CA", "names": {"en": "Canada"}},
        "location": {"latitude": 43.6532, "longitude": -79.3832},
        "subdivisions": [{"names": {"en": "Ontario"}}],
    }),
    ("192.0.2.0/24", {
        "continent": {"code": "NA", "names": {"en": "North America"}},
        "country": {"iso_code": "US", "names": {"en": "United States"}},
    }),
    ("2001:db8::/32", {
        "city": {"names": {"en": "Berlin"}},
        "continent": {"code": "EU", "names": {"en": "Europe"}},
        "country": {"iso_code": "DE", "names": {"en": "Germany"}},
        "subdivisions": [{"names": {"en": "Berlin"}}],
    }),
]


def control(kind, size):
    """The control byte of a field, its extended type byte, and its size bytes."""
    if size < 29:
        bits, tail = size, b""
    elif size < 29 + 256:
        bits, tail = 29, bytes([size - 29])
    elif size < 285 + 65536:
        bits, tail = 30, struct.pack(">H", size - 285)
    else:
        bits, tail = 31, struct.pack(">I", size - 65821)[1:]
    if kind <= 7:
        return bytes([(kind << 5) | bits]) + tail
    return bytes([bits, kind - 7]) + tail


def unsigned(kind, value):
    raw = value.to_bytes((value.bit_length() + 7) // 8, "big") if value else b""
    return control(kind, len(raw)) + raw


def u16(value):
    return unsigned(5, value)


def u32(value):
    return unsigned(6, value)


def u64(value):
    return unsigned(9, value)


def encode(value):
    if isinstance(value, bytes):
        return value  # a number already written with its type
    if isinstance(value, bool):
        return control(14, 1 if value else 0)
    if isinstance(value, str):
        raw = value.encode("utf-8")
        return control(2, len(raw)) + raw
    if isinstance(value, float):
        return control(3, 8) + struct.pack(">d", value)
    if isinstance(value, int):
        return u32(value) if value < 2 ** 32 else u64(value)
    if isinstance(value, dict):
        out = control(7, len(value))
        for key, item in value.items():
            out += encode(key) + encode(item)
        return out
    if isinstance(value, list):
        out = control(11, len(value))
        for item in value:
            out += encode(item)
        return out
    raise TypeError(type(value))


class Node:
    __slots__ = ("left", "right", "number")

    def __init__(self):
        self.left = None
        self.right = None
        self.number = None


def bits_of(network):
    """The bits of a network's prefix in the IPv6 tree: an IPv4 network sits under ::/96."""
    net = ipaddress.ip_network(network)
    value = int(net.network_address)
    prefix = net.prefixlen + (96 if net.version == 4 else 0)
    return [(value >> (127 - i)) & 1 for i in range(prefix)]


def build():
    data = b""
    offsets = []
    for _, record in NETWORKS:
        offsets.append(len(data))
        data += encode(record)

    root = Node()
    for (network, _), offset in zip(NETWORKS, offsets):
        bits = bits_of(network)
        node = root
        for i, bit in enumerate(bits):
            side = "right" if bit else "left"
            here = getattr(node, side)
            if i == len(bits) - 1:
                if here is not None:
                    raise ValueError(f"{network} overlaps another network")
                setattr(node, side, ("data", offset))
            else:
                if here is None:
                    here = Node()
                    setattr(node, side, here)
                elif not isinstance(here, Node):
                    raise ValueError(f"{network} lies inside another network")
                node = here

    order = []
    queue = [root]
    while queue:
        node = queue.pop(0)
        node.number = len(order)
        order.append(node)
        for child in (node.left, node.right):
            if isinstance(child, Node):
                queue.append(child)
    count = len(order)

    def record(child):
        if child is None:
            return count
        if isinstance(child, Node):
            return child.number
        return count + 16 + child[1]

    tree = b"".join(record(n.left).to_bytes(3, "big") + record(n.right).to_bytes(3, "big") for n in order)
    metadata = encode({
        "binary_format_major_version": u16(2),
        "binary_format_minor_version": u16(0),
        "build_epoch": u64(1764547200),
        "database_type": "DBIP-City-Lite",
        "description": {"en": "Singularr test locations"},
        "ip_version": u16(6),
        "languages": ["en"],
        "node_count": u32(count),
        "record_size": u16(24),
    })
    return tree + b"\x00" * 16 + data + b"\xab\xcd\xefMaxMind.com" + metadata, count, offsets


def walk(blob, count, address):
    """The data offset an address reaches in the tree, or None: a check of the written file."""
    ip = ipaddress.ip_address(address)
    value = int(ip)
    node = 0
    for i in range(128):
        bit = (value >> (127 - i)) & 1
        start = node * 6 + (3 if bit else 0)
        node = int.from_bytes(blob[start:start + 3], "big")
        if node >= count:
            return None if node == count else node - count - 16
    return None


def main():
    out_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "test-city.mmdb")
    blob, count, offsets = build()
    for address, expected in [("::203.0.113.7", offsets[0]), ("::198.51.100.20", offsets[1]),
                              ("::192.0.2.1", offsets[2]), ("2001:db8::5", offsets[3]),
                              ("::8.8.8.8", None), ("2001:db9::1", None)]:
        found = walk(blob, count, address)
        if found != expected:
            raise SystemExit(f"{address}: found {found}, expected {expected}")
    with open(out_path, "wb") as f:
        f.write(blob)
    print(f"{out_path}: {len(blob)} bytes, {count} nodes")


if __name__ == "__main__":
    main()

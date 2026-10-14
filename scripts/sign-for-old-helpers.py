#!/usr/bin/env python3
"""Signs a Windows installer so that helpers from 1.8.6 to 1.9.0 hand it to
the installer that asks for permission, instead of installing it themselves.

Those helpers (revisions 9 to 11) start the installer with a command line
that cmd never runs, so their silent update never happened: the app closed
and opened the same old version again. They only install an update whose
signature names its version ("version:" in the trusted comment). Signed
without it, they refuse, and the app falls back to the normal installer
(one Windows permission prompt), which works.

The signature is a normal minisign signature with the update key, so the
app's updater accepts it as usual. Only this one release needs it: newer
helpers start the installer correctly.

    python3 scripts/sign-for-old-helpers.py KEY_FILE INSTALLER [LATEST_JSON]

KEY_FILE is the updater key as Tauri writes it (base64 of an encrypted
minisign key, empty password). Writes INSTALLER.sig, and with LATEST_JSON
puts the new signature into its Windows entries. Nothing is written unless
both signatures check out against the public key in tauri.conf.json.
"""
import base64
import hashlib
import json
import os
import struct
import sys
import time

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

HERE = os.path.dirname(os.path.abspath(__file__))


def scrypt_params(opslimit, memlimit):
    """libsodium's crypto_pwhash_scryptsalsa208sha256 parameter choice (what minisign uses)."""
    opslimit = max(opslimit, 32768)
    r = 8
    if opslimit < memlimit // 32:
        p = 1
        max_n = opslimit // (r * 4)
        n_log2 = next(i for i in range(1, 63) if (1 << i) > max_n // 2)
    else:
        max_n = memlimit // (r * 128)
        n_log2 = next(i for i in range(1, 63) if (1 << i) > max_n // 2)
        max_rp = min((opslimit // 4) // (1 << n_log2), 0x3FFFFFFF)
        p = max_rp // r
    return 1 << n_log2, r, p


def secret_key(path, password=b""):
    text = base64.b64decode(open(path).read().strip()).decode()
    box = base64.b64decode(text.strip().split("\n")[1])
    alg, kdf, cksum = box[0:2], box[2:4], box[4:6]
    salt = box[6:38]
    opslimit, memlimit = struct.unpack("<QQ", box[38:54])
    keynum = box[54:158]
    if alg != b"Ed" or cksum != b"B2":
        sys.exit("not a minisign Ed25519 key")
    if kdf == b"Sc":
        n, r, p = scrypt_params(opslimit, memlimit)
        stream = hashlib.scrypt(password, salt=salt, n=n, r=r, p=p, maxmem=2**31 - 1, dklen=len(keynum))
        keynum = bytes(a ^ b for a, b in zip(keynum, stream))
    key_id, sk, check = keynum[0:8], keynum[8:72], keynum[72:104]
    if hashlib.blake2b(alg + key_id + sk, digest_size=32).digest() != check:
        sys.exit("the key didn't open (wrong password?)")
    return key_id, sk


def public_key():
    conf = json.load(open(os.path.join(HERE, "..", "src-tauri", "tauri.conf.json")))
    text = base64.b64decode(conf["plugins"]["updater"]["pubkey"]).decode()
    raw = base64.b64decode(text.strip().split("\n")[1])
    return raw[2:10], raw[10:42]


def sign(key_id, sk, data, trusted_comment):
    private = Ed25519PrivateKey.from_private_bytes(sk[:32])
    # "ED": the prehashed form Tauri signs with.
    signature = private.sign(hashlib.blake2b(data).digest())
    global_signature = private.sign(signature + trusted_comment.encode())
    text = (
        "untrusted comment: signature from tauri secret key\n"
        + base64.b64encode(b"ED" + key_id + signature).decode()
        + "\ntrusted comment: "
        + trusted_comment
        + "\n"
        + base64.b64encode(global_signature).decode()
        + "\n"
    )
    return base64.b64encode(text.encode()).decode()


def check(signature_b64, data):
    """The same checks as the app's updater (minisign): the file, then the trusted comment."""
    pub_id, pub = public_key()
    lines = base64.b64decode(signature_b64).decode().split("\n")
    raw = base64.b64decode(lines[1])
    comment = lines[2].removeprefix("trusted comment: ")
    if raw[:2] != b"ED" or raw[2:10] != pub_id:
        sys.exit("signed with another key")
    key = Ed25519PublicKey.from_public_bytes(pub)
    try:
        key.verify(raw[10:74], hashlib.blake2b(data).digest())
        key.verify(base64.b64decode(lines[3]), raw[10:74] + comment.encode())
    except InvalidSignature:
        sys.exit("the new signature doesn't check out")
    return comment


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    key_path, installer = sys.argv[1], sys.argv[2]
    data = open(installer, "rb").read()
    key_id, sk = secret_key(key_path)
    comment = f"timestamp:{int(time.time())}\tfile:{os.path.basename(installer)}"
    signature = sign(key_id, sk, data, comment)
    print("checked:", check(signature, data).replace("\t", "  "))
    open(installer + ".sig", "w").write(signature)
    if len(sys.argv) > 3:
        latest = json.load(open(sys.argv[3]))
        changed = [k for k in latest["platforms"] if k.startswith("windows")]
        for k in changed:
            latest["platforms"][k]["signature"] = signature
        json.dump(latest, open(sys.argv[3], "w"), indent=2)
        print("latest.json:", ", ".join(changed))


if __name__ == "__main__":
    main()

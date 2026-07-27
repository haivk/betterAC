#!/usr/bin/env python3
"""Turn Decal's MSI Registry table into the committed template `decal.reg.in`.

Decal's installer is not run. Its COM registration is not self-registration either
-- the MSI carries it as ~1900 plain rows in a `Registry` table, so we can reproduce
the whole thing with a .reg import and skip regsvr32, RegAsm, the GAC and .NET
entirely. This script does that translation once, at development time; `decal.rs`
only substitutes two paths and imports the result.

Two things it has to get right:

* **Formatted values.** MSI values contain `[INSTALLDIR]`, `[#FileKey]`,
  `[ADAPTERVERSION]` and friends, resolved against the Directory/Component/File
  tables. Versions are baked in (the MSI is pinned); the two real paths become the
  placeholders `@@INSTALLDIR@@` and `@@PORTALPATH@@`, each of which stands for a
  directory *including* its trailing separator, matching MSI semantics.

  The substituted values must arrive ALREADY .reg-escaped (`\\\\` for a backslash),
  because everything around them in this file is escaped and the placeholders
  themselves contain no backslashes to escape.

* **Both WoW64 views.** The prefix is win64 and Decal is 32-bit, and Wine applies
  registry redirection in the server -- so a 32-bit COM client reads
  `Software\\Classes\\Wow6432Node\\CLSID\\...` while `wine reg query` (64-bit)
  reads the unredirected path. Every row is therefore written to both views.
  Duplication is inert: a given client only ever reads one of them.

Usage:  python3 genreg.py /path/to/Decal.msi decal.reg.in
Needs `msiinfo` (brew install msitools) -- development-time only, never at runtime.
"""
import csv
import io
import re
import subprocess
import sys

MSI, OUT = sys.argv[1], sys.argv[2]

# Versions are pinned with the MSI, so they are baked in rather than templated.
VERSIONS = {
    "ADAPTERVERSION": "2.9.8.3",
    "PIAVERSION": "2.9.8.3",
    "CLRVERSION": "v4.0.30319",
    "EXPECTEDFRAMEWORK": "30319",
    "ProductName": "Decal 3.0 (2.9.8.3)",
    "ProductVersion": "2.9.0803",
}
# Directories that become placeholders, each standing for a path WITH its trailing
# separator. PIADIR and DEBUGDIR hang off INSTALLDIR, so they template for free.
PLACEHOLDER_DIRS = {
    "INSTALLDIR": "@@INSTALLDIR@@",
    "PIADIR": "@@INSTALLDIR@@.NET 4.0 PIA\\",
    "DEBUGDIR": "@@INSTALLDIR@@Decal.Adapter (Debug)\\",
}


def table(name):
    """Export one MSI table as dicts (msiinfo emits 3 header lines)."""
    raw = subprocess.run(
        ["msiinfo", "export", MSI, name], capture_output=True, text=True, check=True
    ).stdout
    rows = list(csv.reader(io.StringIO(raw), delimiter="\t"))
    return [dict(zip(rows[0], r)) for r in rows[3:] if r]


directory = {r["Directory"]: (r["Directory_Parent"], r["DefaultDir"]) for r in table("Directory")}
component = {r["Component"]: r["Directory_"] for r in table("Component")}
files = {r["File"]: (r["Component_"], r["FileName"]) for r in table("File")}


def resolve_dir(key):
    """A directory as a path ending in a separator, or its placeholder."""
    if key in PLACEHOLDER_DIRS:
        return PLACEHOLDER_DIRS[key]
    parent, default = directory[key]
    name = default.split(":")[-1].split("|")[-1]  # "short|long" -> long
    return resolve_dir(parent) + name + "\\"


def resolve_file(key):
    comp, name = files[key]
    return resolve_dir(component[comp]) + name.split("|")[-1]


def fmt(value):
    """Resolve MSI's Formatted type: [PROPERTY], [#FileKey], [\\x], [~]."""

    def sub(m):
        tok = m.group(1)
        if tok.startswith("#"):
            return resolve_file(tok[1:])
        if tok.startswith("\\"):
            return tok[1:]
        if tok == "~":
            return "\0"
        if tok == "PORTALPATH":
            return "@@PORTALPATH@@"
        return VERSIONS.get(tok, PLACEHOLDER_DIRS.get(tok, m.group(0)))

    return re.sub(r"\[([^\]]*)\]", sub, value)


def esc(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')


def targets(root, key):
    """Where a row must be written so BOTH a 32- and 64-bit client find it."""
    if root == "0":  # HKCR lives under HKLM\Software\Classes
        return [
            f"HKEY_LOCAL_MACHINE\\Software\\Classes\\{key}",
            f"HKEY_LOCAL_MACHINE\\Software\\Classes\\Wow6432Node\\{key}",
        ]
    if root in ("2", "-1") and key.lower().startswith("software\\"):
        rest = key[len("software\\") :]
        return [
            f"HKEY_LOCAL_MACHINE\\Software\\{rest}",
            f"HKEY_LOCAL_MACHINE\\Software\\Wow6432Node\\{rest}",
        ]
    hive = {"1": "HKEY_CURRENT_USER", "3": "HKEY_USERS"}[root]
    return [f"{hive}\\{key}"]


keys = {}  # key -> [value lines], insertion-ordered
for r in table("Registry"):
    root, key, name, val = r["Root"], fmt(r["Key"]), r["Name"], r["Value"]
    where = targets(root, key)
    for full in where:
        keys.setdefault(full, [])
    if name in ("+", "-", "*"):  # create/remove-key markers carry no value
        continue
    val = fmt(val)
    # Everything ships OFF. The MSI enables the Hotkey System plugin (Enabled=#1);
    # force every plugin's Enabled to 0 in the template so a fresh install has no
    # plugin running until the user turns one on. Doing it here, deterministically,
    # means the installer needs no post-install "disable everything" pass (which had
    # to query the live registry and could race a settling wineserver).
    if name.lower() == "enabled" and "\\decal\\plugins\\" in key.lower():
        val = "#0"
    if val.startswith("#x"):
        body = val[2:]
        rhs = "hex:" + ",".join(body[i : i + 2] for i in range(0, len(body), 2))
    elif val.startswith("#%"):
        rhs = "hex(2):" + ",".join(f"{b:02x}" for b in (val[2:] + "\0").encode("utf-16-le"))
    elif val.startswith("#") and val[1:].lstrip("-").isdigit():
        rhs = "dword:%08x" % (int(val[1:]) & 0xFFFFFFFF)
    else:
        rhs = f'"{esc(val)}"'
    lhs = "@" if not name else f'"{esc(name)}"'
    for full in where:
        keys[full].append(f"{lhs}={rhs}")

out = [
    "Windows Registry Editor Version 5.00",
    "",
    "; GENERATED by helpers/decal/genreg.py from Decal.msi -- do not edit by hand.",
    "; @@INSTALLDIR@@ and @@PORTALPATH@@ are substituted at install time with",
    "; .reg-escaped paths (backslashes already doubled).",
    "",
]
for k, vals in keys.items():
    out.append(f"[{k}]")
    out.extend(vals)
    out.append("")

# UTF-8 here so the template diffs readably in git; decal.rs re-encodes to the
# UTF-16LE-with-BOM that regedit expects when it writes the substituted copy.
open(OUT, "w", encoding="utf-8", newline="").write("\r\n".join(out))
print(f"{len(keys)} keys, {sum(len(v) for v in keys.values())} values -> {OUT}")

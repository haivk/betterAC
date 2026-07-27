/* decinject.exe — start Asheron's Call with Decal loaded into it.
 *
 *   decinject.exe C:\path\to\acclient.exe <client args...>
 *
 * Decal normally gets into the client through DenAgent, its MFC config GUI, which
 * arms a global SetWindowsHookEx so the hook DLL lands in the next process that
 * starts. betterAC replaces DenAgent, so it injects directly instead: start the
 * client suspended, load Inject.dll into it, call Inject.dll's exported
 * DecalStartup(), then let it run. That is deterministic, needs no global hook, and
 * needs no .NET — Decal's whole native stack (Decal.dll, decalnet, decalrender,
 * DecalFilters, D3DService…) comes up behind Inject.dll.
 *
 * Suspended matters: DecalStartup installs Decal's Direct3D hooks, and those have
 * to be in place before AC creates its device.
 *
 * If anything about the injection fails we still run the game unhooked — losing
 * plugins beats refusing to launch.
 *
 * Build:  i686-w64-mingw32-gcc -O2 -s -o decinject.exe decinject.c
 * (32-bit on purpose: the client, Decal and this all have to agree.)
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

/* Where Decal was installed, from the registry we wrote at install time rather
 * than a hardcoded path. This binary is 32-bit, so it reads the same redirected
 * view Decal's own COM registration lives in. */
static BOOL decal_dir(char *out, DWORD cb)
{
    HKEY k;
    if (RegOpenKeyExA(HKEY_LOCAL_MACHINE, "Software\\Decal\\Agent", 0, KEY_READ, &k))
        return FALSE;
    DWORD type = 0, len = cb;
    LONG r = RegQueryValueExA(k, "AgentPath", NULL, &type, (BYTE *)out, &len);
    RegCloseKey(k);
    return r == ERROR_SUCCESS && type == REG_SZ && len > 1;
}

/* Rebuild a command line from argv, quoting anything with a space. AC's own
 * arguments carry the account and password, which can contain anything. */
static void build_cmdline(char *out, size_t cb, int argc, char **argv)
{
    out[0] = 0;
    for (int i = 1; i < argc; i++) {
        BOOL quote = strchr(argv[i], ' ') != NULL;
        if (i > 1)
            strncat(out, " ", cb - strlen(out) - 1);
        if (quote)
            strncat(out, "\"", cb - strlen(out) - 1);
        strncat(out, argv[i], cb - strlen(out) - 1);
        if (quote)
            strncat(out, "\"", cb - strlen(out) - 1);
    }
}

/* Load a DLL into the target by full path; returns the remote HMODULE (the
 * 32-bit exit-code-is-HMODULE trick), 0 on failure. Fire-and-forget helper. */
static DWORD load_remote(HANDLE proc, const char *path)
{
    SIZE_T len = strlen(path) + 1;
    void *remote = VirtualAllocEx(proc, NULL, len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if (!remote || !WriteProcessMemory(proc, remote, path, len, NULL))
        return 0;
    LPTHREAD_START_ROUTINE loadlib =
        (LPTHREAD_START_ROUTINE)GetProcAddress(GetModuleHandleA("kernel32.dll"), "LoadLibraryA");
    HANDLE t = CreateRemoteThread(proc, NULL, 0, loadlib, remote, 0, NULL);
    if (!t)
        return 0;
    WaitForSingleObject(t, 30000);
    DWORD base = 0;
    GetExitCodeThread(t, &base);
    CloseHandle(t);
    return base;
}

/* Load Inject.dll into the target and run DecalStartup there. */
static void inject(HANDLE proc, const char *dir)
{
    /* First: give ole32!CoInitialize a hookable facade, before Decal touches
     * anything. Decal only hooks a function whose prologue is the MSVC
     * hot-patch stub; Wine's builtin ole32 exposes a plain prologue, so Decal's
     * CoInitialize hook -- which drives its COM-dependent init (the switchbar
     * control and plugin instantiation) -- would silently fail to install.
     * cohook.dll repoints CoInitialize at a 90 90 trampoline so Decal's own
     * installer accepts it. Best-effort: if absent we still inject (no plugins,
     * same as before). */
    char cohook[MAX_PATH];
    snprintf(cohook, sizeof cohook, "%scohook.dll", dir);
    if (!load_remote(proc, cohook))
        fprintf(stderr, "decinject: cohook.dll not loaded; Decal COM init may not fire\n");

    char dll[MAX_PATH];
    snprintf(dll, sizeof dll, "%sInject.dll", dir);

    /* DONT_RESOLVE_DLL_REFERENCES maps the image without running its DllMain, so
     * this launcher never becomes a second Decal host -- we only want the RVA. */
    HMODULE local = LoadLibraryExA(dll, NULL, DONT_RESOLVE_DLL_REFERENCES);
    if (!local) {
        fprintf(stderr, "decinject: cannot read %s (err %lu)\n", dll, GetLastError());
        return;
    }
    FARPROC startup = GetProcAddress(local, "DecalStartup");
    if (!startup) {
        fprintf(stderr, "decinject: Inject.dll has no DecalStartup\n");
        return;
    }
    DWORD rva = (DWORD)((BYTE *)startup - (BYTE *)local);

    SIZE_T len = strlen(dll) + 1;
    void *remote = VirtualAllocEx(proc, NULL, len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if (!remote || !WriteProcessMemory(proc, remote, dll, len, NULL)) {
        fprintf(stderr, "decinject: cannot stage the path (err %lu)\n", GetLastError());
        return;
    }

    LPTHREAD_START_ROUTINE loadlib =
        (LPTHREAD_START_ROUTINE)GetProcAddress(GetModuleHandleA("kernel32.dll"), "LoadLibraryA");
    HANDLE t = CreateRemoteThread(proc, NULL, 0, loadlib, remote, 0, NULL);
    if (!t) {
        fprintf(stderr, "decinject: CreateRemoteThread failed (err %lu)\n", GetLastError());
        return;
    }
    WaitForSingleObject(t, 30000);
    /* In a 32-bit process the thread's exit code IS the HMODULE LoadLibraryA
     * returned, which is how we learn where Inject.dll landed. */
    DWORD base = 0;
    GetExitCodeThread(t, &base);
    CloseHandle(t);
    if (!base) {
        fprintf(stderr, "decinject: the target could not load Inject.dll\n");
        return;
    }

    HANDLE t2 = CreateRemoteThread(proc, NULL, 0,
                                   (LPTHREAD_START_ROUTINE)(ULONG_PTR)(base + rva),
                                   NULL, 0, NULL);
    if (!t2) {
        fprintf(stderr, "decinject: could not call DecalStartup (err %lu)\n", GetLastError());
        return;
    }
    WaitForSingleObject(t2, 30000);
    CloseHandle(t2);
}

int main(int argc, char **argv)
{
    if (argc < 2) {
        fprintf(stderr, "usage: decinject.exe <client.exe> [args...]\n");
        return 2;
    }

    char dir[MAX_PATH] = {0};
    BOOL have_decal = decal_dir(dir, sizeof dir);
    if (!have_decal)
        fprintf(stderr, "decinject: Decal is not installed; launching unhooked\n");

    /* The launch paths name the client as a bare "acclient.exe", relative to the
     * game directory they set as our working directory. Resolve it to a full path
     * against that cwd: CreateProcess's own search for a bare name does not
     * reliably include the current directory under Wine, and AC also wants the cwd
     * to be its own folder to find its data files. */
    char exe[MAX_PATH];
    if (strpbrk(argv[1], "\\/")) {
        strncpy(exe, argv[1], sizeof exe - 1);
        exe[sizeof exe - 1] = 0;
    } else {
        DWORD n = GetCurrentDirectoryA(sizeof exe, exe);
        snprintf(exe + n, sizeof exe - n, "\\%s", argv[1]);
    }

    /* The client's working directory is its own folder. */
    char cwd[MAX_PATH];
    strncpy(cwd, exe, sizeof cwd - 1);
    cwd[sizeof cwd - 1] = 0;
    char *slash = strrchr(cwd, '\\');
    if (slash)
        *slash = 0;

    char cmd[8192];
    build_cmdline(cmd, sizeof cmd, argc, argv);

    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    ZeroMemory(&si, sizeof si);
    si.cb = sizeof si;
    /* Name the exe explicitly (first arg) so there is no search, and keep the full
     * command line (second arg) so the client still sees argv[0] plus its own
     * arguments. Inherit handles so the client's own output reaches whoever
     * launched us -- without this a working run looks completely silent. */
    if (!CreateProcessA(exe, cmd, NULL, NULL, TRUE, CREATE_SUSPENDED, NULL,
                        slash ? cwd : NULL, &si, &pi)) {
        fprintf(stderr, "decinject: cannot start %s (err %lu)\n", exe, GetLastError());
        return 1;
    }

    /* Inject BEFORE the client runs, while it is still suspended. DecalStartup
     * installs Decal's Direct3D create/device hooks, and those must be in place
     * before the client creates its D3D device -- otherwise Decal never sees the
     * device, so its overlay (the switchbar) never draws and its plugins never
     * start. (Injecting later, after the window is up, was tried while chasing a
     * crash; it left Decal loaded but inert, and the crash was really a missing
     * CLR, not injection timing.) */
    if (have_decal)
        inject(pi.hProcess, dir);

    ResumeThread(pi.hThread);
    WaitForSingleObject(pi.hProcess, INFINITE);

    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return (int)code;
}

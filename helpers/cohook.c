/* cohook.dll — make ole32!CoInitialize hookable by Decal on Wine.
 *
 * Decal installs its API hooks by patching a target function whose first two
 * bytes are 90 90 (MSVC hot-patch signature). Wine's builtin d3d9/kernel32
 * expose `8b ff` (mov edi,edi) which we normalize to 90 90 in the engine DLLs.
 * But Wine's ole32!CoInitialize has NO hot-patch stub (`55 89 e5 ...`), so it
 * can't be normalized in place. Decal needs that hook to run its COM-dependent
 * init (the switchbar control + plugin instantiation).
 *
 * Fix: build a small trampoline T that STARTS with 90 90, then repoint
 * CoInitialize's entry at T with a `ff 25` (jmp dword ptr [P]) indirect jump.
 * Decal's target resolver follows `ff 25` unconditionally, lands on T, sees
 * 90 90, and hot-patches T normally. Execution flows entry -> T -> (Decal hook
 * or, when Decal isn't present, the relocated prologue) -> CoInitialize+6.
 *
 * Must be loaded BEFORE DecalStartup runs. Idempotent.
 *
 * Build: i686-w64-mingw32-gcc -O2 -s -shared -o cohook.dll cohook.c
 */
#include <windows.h>

/* CoInitialize on this Wine build: 55 89 e5 83 ec 08 (push ebp; mov ebp,esp;
 * sub esp,8) — 3 instructions, 6 bytes. We relocate these 6 and jmp back to +6. */
#define RELOC_N 6

static int detoured = 0;

static void install(void)
{
    if (detoured) return;
    HMODULE ole = LoadLibraryA("ole32.dll");
    if (!ole) return;
    BYTE *co = (BYTE *)GetProcAddress(ole, "CoInitialize");
    if (!co) return;

    /* Already façaded (ff 25 / e9 / 90 90)? then nothing to do. */
    if (co[0] == 0xFF || co[0] == 0xE9 || (co[0] == 0x90 && co[1] == 0x90)) { detoured = 1; return; }
    /* Only proceed on the exact prologue we expect, so we never corrupt an
     * unexpected build. */
    if (!(co[0] == 0x55 && co[1] == 0x89 && co[2] == 0xE5 &&
          co[3] == 0x83 && co[4] == 0xEC && co[5] == 0x08)) return;

    /* One page holds both the pointer slot P and the trampoline T. */
    BYTE *page = (BYTE *)VirtualAlloc(NULL, 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    if (!page) return;
    DWORD *P = (DWORD *)page;      /* [0..3]  : holds &T                     */
    BYTE  *T = page + 8;           /* [8..]   : 90 90 <reloc 6> E9 <rel +6>  */

    T[0] = 0x90; T[1] = 0x90;                 /* hookable stub               */
    for (int i = 0; i < RELOC_N; i++) T[2 + i] = co[i];   /* relocated prologue */
    T[2 + RELOC_N] = 0xE9;                                 /* jmp back        */
    *(DWORD *)(T + 2 + RELOC_N + 1) = (DWORD)((co + RELOC_N) - (T + 2 + RELOC_N + 5));
    *P = (DWORD)(ULONG_PTR)T;

    /* Repoint CoInitialize: ff 25 <&P> = jmp dword ptr [P]  (6 bytes == RELOC_N) */
    DWORD old;
    if (!VirtualProtect(co, 8, PAGE_EXECUTE_READWRITE, &old)) return;
    co[0] = 0xFF; co[1] = 0x25;
    *(DWORD *)(co + 2) = (DWORD)(ULONG_PTR)P;
    VirtualProtect(co, 8, old, NULL);
    FlushInstructionCache(GetCurrentProcess(), co, 8);
    detoured = 1;
}

BOOL WINAPI DllMain(HINSTANCE h, DWORD r, LPVOID x)
{
    (void)x;
    if (r == DLL_PROCESS_ATTACH) { DisableThreadLibraryCalls(h); install(); }
    return TRUE;
}

// version.dll proxy loader for Flashes of Chaos.
//
// The game imports VerQueryValueW / GetFileVersionInfoSizeW / GetFileVersionInfoW
// from VERSION.dll. Windows (and Wine with WINEDLLOVERRIDES="version=n,b") loads
// this file from the game folder first. We forward every VERSION.dll export to
// the real system DLL and, on a separate thread, load focmp.dll from our own
// folder and call its `focmp_init` export.
//
// Build: x86_64-w64-mingw32-gcc -O2 -shared -o version.dll version.c version.def -s

#include <windows.h>

#define EXPORTS(X)                   \
    X(GetFileVersionInfoA)           \
    X(GetFileVersionInfoByHandle)    \
    X(GetFileVersionInfoExA)         \
    X(GetFileVersionInfoExW)         \
    X(GetFileVersionInfoSizeA)       \
    X(GetFileVersionInfoSizeExA)     \
    X(GetFileVersionInfoSizeExW)     \
    X(GetFileVersionInfoSizeW)       \
    X(GetFileVersionInfoW)           \
    X(VerFindFileA)                  \
    X(VerFindFileW)                  \
    X(VerInstallFileA)               \
    X(VerInstallFileW)               \
    X(VerLanguageNameA)              \
    X(VerLanguageNameW)              \
    X(VerQueryValueA)                \
    X(VerQueryValueW)

// One pointer per export, plus an asm trampoline `Name_fwd` that tail-jumps
// through it. Arguments pass through untouched, so no prototypes are needed.
// The .def file exports `Name = Name_fwd`.
#define DECL_PTR(name) void *p_##name = 0;
EXPORTS(DECL_PTR)

#define TRAMPOLINE(name)                              \
    __asm__(".globl " #name "_fwd\n"                  \
            #name "_fwd:\n"                           \
            "    jmp *p_" #name "(%rip)\n");
EXPORTS(TRAMPOLINE)

static HMODULE g_self;

static void log_line(const char *msg) {
    OutputDebugStringA(msg);
    wchar_t path[MAX_PATH];
    DWORD n = GetModuleFileNameW(g_self, path, MAX_PATH);
    while (n > 0 && path[n - 1] != L'\\' && path[n - 1] != L'/') n--;
    path[n] = 0;
    lstrcatW(path, L"focmp_loader.log");
    HANDLE f = CreateFileW(path, FILE_APPEND_DATA, FILE_SHARE_READ, NULL, OPEN_ALWAYS,
                           FILE_ATTRIBUTE_NORMAL, NULL);
    if (f != INVALID_HANDLE_VALUE) {
        DWORD w;
        WriteFile(f, msg, lstrlenA(msg), &w, NULL);
        WriteFile(f, "\r\n", 2, &w, NULL);
        CloseHandle(f);
    }
}

static void resolve_real(void) {
    wchar_t path[MAX_PATH];
    UINT n = GetSystemDirectoryW(path, MAX_PATH);
    if (n == 0 || n > MAX_PATH - 16) return;
    lstrcatW(path, L"\\version.dll");
    HMODULE real = LoadLibraryW(path);
    if (!real) {
        log_line("focmp loader: FAILED to load system version.dll");
        return;
    }
#define RESOLVE(name) p_##name = (void *)GetProcAddress(real, #name);
    EXPORTS(RESOLVE)
    log_line("focmp loader: system version.dll resolved");
}

static DWORD WINAPI load_focmp(LPVOID unused) {
    (void)unused;
    wchar_t path[MAX_PATH];
    DWORD n = GetModuleFileNameW(g_self, path, MAX_PATH);
    while (n > 0 && path[n - 1] != L'\\' && path[n - 1] != L'/') n--;
    path[n] = 0;
    lstrcatW(path, L"focmp.dll");
    HMODULE m = LoadLibraryW(path);
    if (!m) {
        char buf[96];
        wsprintfA(buf, "focmp loader: LoadLibrary(focmp.dll) failed, error %lu", GetLastError());
        log_line(buf);
        return 1;
    }
    typedef void(__cdecl * init_fn)(void);
    init_fn init = (init_fn)GetProcAddress(m, "focmp_init");
    if (!init) {
        log_line("focmp loader: focmp.dll has no focmp_init export");
        return 1;
    }
    log_line("focmp loader: calling focmp_init");
    init();
    return 0;
}

BOOL WINAPI DllMain(HINSTANCE inst, DWORD reason, LPVOID reserved) {
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) {
        g_self = inst;
        DisableThreadLibraryCalls(inst);
        resolve_real();
        // LoadLibrary of focmp.dll waits for the loader lock, so it runs only
        // after process startup has finished.
        HANDLE t = CreateThread(NULL, 0, load_focmp, NULL, 0, NULL);
        if (t) CloseHandle(t);
    }
    return TRUE;
}

/**
 * bcryptprimitives.dll shim for Windows 7 compatibility
 *
 * Rust's standard library statically imports ProcessPrng from
 * bcryptprimitives.dll via raw-dylib. On Windows 7, bcryptprimitives.dll
 * exists but does not export ProcessPrng (added in Windows 8).
 *
 * This shim DLL is placed alongside the exe. The Windows loader finds it
 * first (DLL search order: exe directory before system32). On Windows 8+,
 * ProcessPrng is forwarded to the real bcryptprimitives.dll from system32.
 * On Windows 7, it falls back to RtlGenRandom (SystemFunction036) from
 * advapi32.dll, which is available since Windows XP.
 *
 * NOTE: Only ProcessPrng is exported here. System DLLs that need the real
 * bcryptprimitives.dll use KnownDLLs or absolute paths and are unaffected
 * by this shim.
 *
 * Compile with MSVC (x64):
 *   cl /LD /O2 /W2 /nologo bcryptprimitives.c /link /DEF:bcryptprimitives.def /OUT:bcryptprimitives.dll
 */

#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>

/* ProcessPrng: BOOL WINAPI ProcessPrng(PUCHAR pbData, SIZE_T cbData)
 * RtlGenRandom (SystemFunction036): BOOLEAN WINAPI RtlGenRandom(PVOID, ULONG) */
typedef BOOL(WINAPI *pfn_ProcessPrng)(PUCHAR pbData, SIZE_T cbData);
typedef BOOL(WINAPI *pfn_RtlGenRandom)(PVOID RandomBuffer, ULONG RandomBufferLength);

static pfn_ProcessPrng g_native_ProcessPrng = NULL;
static pfn_RtlGenRandom g_fallback_RtlGenRandom = NULL;

static volatile LONG g_init_done = 0;
static CRITICAL_SECTION g_init_cs;
static HINSTANCE g_self_module = NULL;

static void ShimInit(void)
{
  if (InterlockedCompareExchange(&g_init_done, 0, 0) == 1)
    return;

  EnterCriticalSection(&g_init_cs);
  if (g_init_done == 0)
  {
    /* Try to load the REAL bcryptprimitives.dll from system32.
     * Use absolute path to avoid loading ourselves recursively. */
    char sys_path[MAX_PATH];
    UINT n = GetSystemDirectoryA(sys_path, MAX_PATH);
    if (n > 0 && n < (MAX_PATH - 30))
    {
      lstrcatA(sys_path, "\\bcryptprimitives.dll");
      HMODULE hReal = LoadLibraryExA(sys_path, NULL,
                                     LOAD_WITH_ALTERED_SEARCH_PATH);
      if (hReal != NULL && hReal != g_self_module)
      {
        g_native_ProcessPrng = (pfn_ProcessPrng)
            GetProcAddress(hReal, "ProcessPrng");
      }
    }

    if (g_native_ProcessPrng == NULL)
    {
      /* Win7 fallback: use RtlGenRandom (= SystemFunction036) from advapi32 */
      HMODULE hAdvapi = LoadLibraryA("advapi32.dll");
      if (hAdvapi)
      {
        g_fallback_RtlGenRandom = (pfn_RtlGenRandom)
            GetProcAddress(hAdvapi, "SystemFunction036");
      }
    }

    InterlockedExchange(&g_init_done, 1);
  }
  LeaveCriticalSection(&g_init_cs);
}

/* ======== Exported API ======== */

__declspec(dllexport) BOOL WINAPI ProcessPrng(PUCHAR pbData, SIZE_T cbData)
{
  ShimInit();

  if (g_native_ProcessPrng)
    return g_native_ProcessPrng(pbData, cbData);

  /* Win7: RtlGenRandom only accepts ULONG length; call in chunks if needed */
  if (g_fallback_RtlGenRandom)
  {
    SIZE_T remaining = cbData;
    PUCHAR p = pbData;
    while (remaining > 0)
    {
      ULONG chunk = (remaining > 0xFFFFFFFFUL)
                        ? 0xFFFFFFFFUL
                        : (ULONG)remaining;
      if (!g_fallback_RtlGenRandom(p, chunk))
        return FALSE;
      p += chunk;
      remaining -= chunk;
    }
    return TRUE;
  }

  /* Last resort: should never reach here on any supported Windows version */
  SetLastError(ERROR_NOT_SUPPORTED);
  return FALSE;
}

/* ======== DLL entry point ======== */

BOOL WINAPI DllMain(HINSTANCE hinstDLL, DWORD fdwReason, LPVOID lpvReserved)
{
  (void)lpvReserved;
  switch (fdwReason)
  {
  case DLL_PROCESS_ATTACH:
    g_self_module = hinstDLL;
    InitializeCriticalSection(&g_init_cs);
    DisableThreadLibraryCalls(hinstDLL);
    break;
  case DLL_PROCESS_DETACH:
    DeleteCriticalSection(&g_init_cs);
    break;
  }
  return TRUE;
}

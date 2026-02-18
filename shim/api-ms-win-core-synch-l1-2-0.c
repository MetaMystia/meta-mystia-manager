/**
 * api-ms-win-core-synch-l1-2-0.dll shim for Windows 7 compatibility
 *
 * Rust's standard library (and various crates) statically import
 * WaitOnAddress/WakeByAddressAll/WakeByAddressSingle from
 * api-ms-win-core-synch-l1-2-0.dll, which does not exist on Windows 7.
 *
 * This shim DLL is placed alongside the exe. On Windows 7, the loader will
 * find this shim first (DLL search order: exe directory before system32).
 * On Windows 8+, this shim forwards to the real native APIs.
 *
 * Compile with MSVC (x64):
 *   cl /LD /O2 /W2 /nologo api-ms-win-core-synch-l1-2-0.c /link /DEF:api-ms-win-core-synch-l1-2-0.def /OUT:api-ms-win-core-synch-l1-2-0.dll
 */

/*
 * We define _SYNCHAPI_H_ before including windows.h to prevent the SDK's
 * synchapi.h from declaring WaitOnAddress / WakeByAddress*, which would
 * conflict with our own definitions below.
 */
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#define _SYNCHAPI_H_
#include <windows.h>

/* NT Keyed Event types (available XP+, documented in ntifs.h / WDK) */
typedef LONG NTSTATUS;
#define MY_STATUS_TIMEOUT ((NTSTATUS)0x00000102L)

typedef NTSTATUS(WINAPI *pfn_NtCreateKeyedEvent)(
    PHANDLE KeyedEventHandle,
    ACCESS_MASK DesiredAccess,
    PVOID ObjectAttributes,
    ULONG Flags);
typedef NTSTATUS(WINAPI *pfn_NtWaitForKeyedEvent)(
    HANDLE KeyedEventHandle,
    PVOID Key,
    BOOLEAN Alertable,
    PLARGE_INTEGER Timeout);
typedef NTSTATUS(WINAPI *pfn_NtReleaseKeyedEvent)(
    HANDLE KeyedEventHandle,
    PVOID Key,
    BOOLEAN Alertable,
    PLARGE_INTEGER Timeout);

/* Win8+ native function pointer types */
typedef BOOL(WINAPI *pfn_WaitOnAddress)(
    volatile VOID *Address,
    PVOID CompareAddress,
    SIZE_T AddressSize,
    DWORD dwMilliseconds);
typedef VOID(WINAPI *pfn_WakeByAddressSingle)(PVOID Address);
typedef VOID(WINAPI *pfn_WakeByAddressAll)(PVOID Address);

/* ---- Globals ---- */
static HANDLE g_keyed_event = NULL;
static pfn_NtWaitForKeyedEvent g_nt_wait = NULL;
static pfn_NtReleaseKeyedEvent g_nt_release = NULL;

static pfn_WaitOnAddress g_native_wait = NULL;
static pfn_WakeByAddressSingle g_native_wake_single = NULL;
static pfn_WakeByAddressAll g_native_wake_all = NULL;

static volatile LONG g_init_done = 0;
static CRITICAL_SECTION g_init_cs;
static HINSTANCE g_self_module = NULL;

/* One-time initialisation */
static void ShimInit(void)
{
  if (InterlockedCompareExchange(&g_init_done, 0, 0) == 1)
    return;

  EnterCriticalSection(&g_init_cs);
  if (g_init_done == 0)
  {
    /* Try to load the REAL api-ms-win-core-synch-l1-2-0 from system32.
     * We use an absolute path to avoid loading ourselves again. */
    {
      char sys_path[MAX_PATH];
      UINT n = GetSystemDirectoryA(sys_path, MAX_PATH);
      if (n > 0 && n < (MAX_PATH - 50))
      {
        lstrcatA(sys_path, "\\api-ms-win-core-synch-l1-2-0.dll");
        HMODULE hReal = LoadLibraryExA(sys_path, NULL,
                                       LOAD_WITH_ALTERED_SEARCH_PATH);
        /* Make sure we didn't load ourselves */
        if (hReal != NULL && hReal != g_self_module)
        {
          pfn_WaitOnAddress f = (pfn_WaitOnAddress)
              GetProcAddress(hReal, "WaitOnAddress");
          if (f != NULL)
          {
            g_native_wait = f;
            g_native_wake_single = (pfn_WakeByAddressSingle)
                GetProcAddress(hReal, "WakeByAddressSingle");
            g_native_wake_all = (pfn_WakeByAddressAll)
                GetProcAddress(hReal, "WakeByAddressAll");
          }
        }
      }
    }

    if (g_native_wait == NULL)
    {
      /* Fall back to NT Keyed Events (Windows XP / 7) */
      HMODULE hNtdll = GetModuleHandleA("ntdll.dll");
      if (hNtdll)
      {
        pfn_NtCreateKeyedEvent nt_create =
            (pfn_NtCreateKeyedEvent)
                GetProcAddress(hNtdll, "NtCreateKeyedEvent");
        g_nt_wait =
            (pfn_NtWaitForKeyedEvent)
                GetProcAddress(hNtdll, "NtWaitForKeyedEvent");
        g_nt_release =
            (pfn_NtReleaseKeyedEvent)
                GetProcAddress(hNtdll, "NtReleaseKeyedEvent");

        if (nt_create && g_nt_wait && g_nt_release)
        {
          nt_create(&g_keyed_event,
                    GENERIC_READ | GENERIC_WRITE,
                    NULL, 0);
        }
      }
    }

    InterlockedExchange(&g_init_done, 1);
  }
  LeaveCriticalSection(&g_init_cs);
}

/* ---- NT Keyed Event emulation helpers ---- */

static BOOL Shim_WaitOnAddress_Fallback(
    volatile VOID *Address,
    PVOID CompareAddress,
    SIZE_T AddressSize,
    DWORD dwMilliseconds)
{
  if (!g_nt_wait || !g_keyed_event)
    return FALSE;

  /* If value already differs, return immediately */
  BOOL changed = FALSE;
  switch (AddressSize)
  {
  case 1:
    changed = (*(volatile BYTE *)Address != *(BYTE *)CompareAddress);
    break;
  case 2:
    changed = (*(volatile WORD *)Address != *(WORD *)CompareAddress);
    break;
  case 4:
    changed = (*(volatile DWORD *)Address != *(DWORD *)CompareAddress);
    break;
  case 8:
  {
    changed = (*(volatile ULONGLONG *)Address != *(ULONGLONG *)CompareAddress);
    break;
  }
  default:
    break;
  }
  if (changed)
    return TRUE;

  PLARGE_INTEGER pTimeout = NULL;
  LARGE_INTEGER li;
  if (dwMilliseconds != INFINITE)
  {
    li.QuadPart = -(LONGLONG)dwMilliseconds * 10000LL;
    pTimeout = &li;
  }

  NTSTATUS s = g_nt_wait(g_keyed_event, (PVOID)Address, FALSE, pTimeout);
  if (s == MY_STATUS_TIMEOUT)
  {
    SetLastError(ERROR_TIMEOUT);
    return FALSE;
  }
  return TRUE;
}

static VOID Shim_WakeByAddressSingle_Fallback(PVOID Address)
{
  if (!g_nt_release || !g_keyed_event)
    return;
  LARGE_INTEGER zero;
  zero.QuadPart = 0;
  g_nt_release(g_keyed_event, Address, FALSE, &zero);
}

static VOID Shim_WakeByAddressAll_Fallback(PVOID Address)
{
  if (!g_nt_release || !g_keyed_event)
    return;
  LARGE_INTEGER zero;
  zero.QuadPart = 0;
  /* Wake up to a reasonable maximum to avoid unbounded looping.
   * NtReleaseKeyedEvent returns non-success when no waiter is present. */
  int i;
  for (i = 0; i < 4096; ++i)
  {
    if (g_nt_release(g_keyed_event, Address, FALSE, &zero) < 0)
      break; /* no more waiters */
  }
}

/* ======== Exported API ======== */

__declspec(dllexport) BOOL WINAPI WaitOnAddress(
    volatile VOID *Address,
    PVOID CompareAddress,
    SIZE_T AddressSize,
    DWORD dwMilliseconds)
{
  ShimInit();
  if (g_native_wait)
    return g_native_wait(Address, CompareAddress, AddressSize, dwMilliseconds);
  return Shim_WaitOnAddress_Fallback(Address, CompareAddress, AddressSize, dwMilliseconds);
}

__declspec(dllexport) VOID WINAPI WakeByAddressSingle(PVOID Address)
{
  ShimInit();
  if (g_native_wake_single)
    g_native_wake_single(Address);
  else
    Shim_WakeByAddressSingle_Fallback(Address);
}

__declspec(dllexport) VOID WINAPI WakeByAddressAll(PVOID Address)
{
  ShimInit();
  if (g_native_wake_all)
    g_native_wake_all(Address);
  else
    Shim_WakeByAddressAll_Fallback(Address);
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

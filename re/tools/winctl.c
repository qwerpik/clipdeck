/* winctl: inspect/drive Win32 windows inside the Wine prefix (no real input devices).
   winctl tree [titleSubstr]     dump top-level windows (+children) of all processes
   winctl click HWND             BM_CLICK a button
   winctl cmd HWND ID            WM_COMMAND id to a window
   winctl close HWND             WM_CLOSE
   winctl text HWND "str"        WM_SETTEXT
   winctl key HWND VK            WM_KEYDOWN/UP
   winctl menu HWND              dump the window's menu bar recursively (ids + text) */
#include <windows.h>
#include <commctrl.h>
#include <wchar.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static const char *filt = NULL;
static void pr(HWND h, int depth) {
    wchar_t cls[256] = {0}, txt[512] = {0}; char u1[768], u2[1536];
    GetClassNameW(h, cls, 255); GetWindowTextW(h, txt, 511);
    WideCharToMultiByte(CP_UTF8, 0, cls, -1, u1, sizeof u1, 0, 0);
    WideCharToMultiByte(CP_UTF8, 0, txt, -1, u2, sizeof u2, 0, 0);
    RECT r; GetWindowRect(h, &r);
    LONG st = GetWindowLongW(h, GWL_STYLE);
    printf("%*s%p id=%ld cls=%s text=\"%s\" rect=%ld,%ld %ldx%ld vis=%d en=%d style=%08lx\n", depth * 2, "", (void *)h,
           (long)GetDlgCtrlID(h), u1, u2, r.left, r.top, r.right - r.left, r.bottom - r.top,
           IsWindowVisible(h) ? 1 : 0, IsWindowEnabled(h) ? 1 : 0, (unsigned long)st);
    if (!wcscmp(cls, L"ComboBox")) {
        LRESULT n = SendMessageW(h, CB_GETCOUNT, 0, 0), sel = SendMessageW(h, CB_GETCURSEL, 0, 0);
        for (LRESULT i = 0; i < n && i < 200; i++) {
            wchar_t it[512] = {0}; char ui[1536];
            SendMessageW(h, CB_GETLBTEXT, (WPARAM)i, (LPARAM)it);
            WideCharToMultiByte(CP_UTF8, 0, it, -1, ui, sizeof ui, 0, 0);
            printf("%*s  - item[%ld]%s \"%s\"\n", depth * 2, "", (long)i, i == sel ? "*" : "", ui);
        }
    }
    if (!wcscmp(cls, L"msctls_trackbar32")) {
        printf("%*s  - trackbar min=%ld max=%ld pos=%ld\n", depth * 2, "", (long)SendMessageW(h, TBM_GETRANGEMIN, 0, 0),
               (long)SendMessageW(h, TBM_GETRANGEMAX, 0, 0), (long)SendMessageW(h, TBM_GETPOS, 0, 0));
    }
    if (!wcscmp(cls, L"msctls_updown32")) {
        int lo = 0, hi = 0; SendMessageW(h, UDM_GETRANGE32, (WPARAM)&lo, (LPARAM)&hi);
        printf("%*s  - updown min=%d max=%d pos=%ld\n", depth * 2, "", lo, hi, (long)SendMessageW(h, UDM_GETPOS32, 0, 0));
    }
    if (!wcscmp(cls, L"Button") && ((st & 0xF) == BS_AUTOCHECKBOX || (st & 0xF) == BS_AUTORADIOBUTTON || (st & 0xF) == BS_CHECKBOX))
        printf("%*s  - checked=%ld\n", depth * 2, "", (long)SendMessageW(h, BM_GETCHECK, 0, 0));
}
static int g_depth;
static BOOL CALLBACK child(HWND h, LPARAM lp) {
    if (GetParent(h) != (HWND)lp) return TRUE;
    pr(h, g_depth + 1); int d = g_depth; g_depth++; EnumChildWindows(h, child, (LPARAM)h); g_depth = d; return TRUE;
}
static BOOL CALLBACK top(HWND h, LPARAM lp) {
    (void)lp; wchar_t txt[512] = {0}; char u[1536]; GetWindowTextW(h, txt, 511);
    WideCharToMultiByte(CP_UTF8, 0, txt, -1, u, sizeof u, 0, 0);
    if (filt && !strstr(u, filt)) return TRUE;
    if (!filt && !IsWindowVisible(h)) return TRUE;
    pr(h, 0); g_depth = 0; EnumChildWindows(h, child, (LPARAM)h); return TRUE;
}
static void dumpmenu(HMENU m, int depth) {
    int n = GetMenuItemCount(m);
    for (int i = 0; i < n; i++) {
        wchar_t t[512] = {0}; char u[1536];
        MENUITEMINFOW mi = {sizeof mi}; mi.fMask = MIIM_STRING | MIIM_ID | MIIM_SUBMENU | MIIM_FTYPE | MIIM_STATE;
        mi.dwTypeData = t; mi.cch = 511; GetMenuItemInfoW(m, i, TRUE, &mi);
        WideCharToMultiByte(CP_UTF8, 0, t, -1, u, sizeof u, 0, 0);
        printf("%*s[%u] %s%s%s\n", depth * 2, "", mi.wID, (mi.fType & MFT_SEPARATOR) ? "----" : u,
               (mi.fState & MFS_CHECKED) ? " (checked)" : "", (mi.fState & MFS_DISABLED) ? " (disabled)" : "");
        if (mi.hSubMenu) dumpmenu(mi.hSubMenu, depth + 1);
    }
}
int main(int argc, char **argv) {
    if (argc < 2) return 2;
    freopen("Z:\\home\\user\\Projects\\clipdeck\\re\\tools\\winctl.out", "w", stdout);
    HWND h = argc > 2 ? (HWND)(uintptr_t)strtoull(argv[2], 0, 16) : 0;
    if (!strcmp(argv[1], "tree")) { filt = argc > 2 ? argv[2] : NULL; EnumWindows(top, 0); }
    else if (!strcmp(argv[1], "click")) PostMessageW(h, BM_CLICK, 0, 0);
    else if (!strcmp(argv[1], "cmd")) PostMessageW(h, WM_COMMAND, (WPARAM)atoi(argv[3]), 0);
    else if (!strcmp(argv[1], "close")) PostMessageW(h, WM_CLOSE, 0, 0);
    else if (!strcmp(argv[1], "text")) { wchar_t w[1024]; MultiByteToWideChar(CP_UTF8, 0, argv[3], -1, w, 1024); SendMessageW(h, WM_SETTEXT, 0, (LPARAM)w); }
    else if (!strcmp(argv[1], "key")) { int vk = (int)strtol(argv[3], 0, 0); PostMessageW(h, WM_KEYDOWN, vk, 1); PostMessageW(h, WM_KEYUP, vk, 0xC0000001); }
    else if (!strcmp(argv[1], "menu")) { HMENU m = GetMenu(h); if (m) dumpmenu(m, 0); else printf("no menu\n"); }
    return 0;
}

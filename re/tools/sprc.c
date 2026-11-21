/* sprc: send one command to Soundpad's remote-control pipe and print the reply.
   Usage: sprc.exe "GetSoundlist()"  ->  reply on stdout (UTF-8). */
#include <windows.h>
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: sprc CMD\n"); return 2; }
    HANDLE h = CreateFileA("\\\\.\\pipe\\sp_remote_control", GENERIC_READ | GENERIC_WRITE,
                           0, NULL, OPEN_EXISTING, 0, NULL);
    if (h == INVALID_HANDLE_VALUE) { fprintf(stderr, "open failed %lu\n", GetLastError()); return 1; }
    DWORD mode = PIPE_READMODE_MESSAGE;
    SetNamedPipeHandleState(h, &mode, NULL, NULL);
    DWORD n = 0;
    if (!WriteFile(h, argv[1], (DWORD)strlen(argv[1]), &n, NULL)) { fprintf(stderr, "write failed %lu\n", GetLastError()); return 1; }
    static char buf[1 << 22]; DWORD total = 0;
    for (;;) {
        BOOL ok = ReadFile(h, buf + total, sizeof(buf) - 1 - total, &n, NULL);
        total += n;
        if (ok) break;
        if (GetLastError() != ERROR_MORE_DATA) { fprintf(stderr, "read failed %lu\n", GetLastError()); break; }
    }
    fwrite(buf, 1, total, stdout);
    fputc('\n', stdout);
    CloseHandle(h);
    return 0;
}

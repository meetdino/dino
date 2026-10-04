// A stand-in for an agent's process, for tests/found_scan.rs: copied under an agent's name
// (`claude`, `codex`, `dino`…) so the kernel names its process that, and told by its environment
// how to live:
//
//   FAKE_DETACH=1        fork, print the child's pid, and exit: the child's parent becomes launchd,
//                        as for an agent in a terminal app (not under the test, nor what runs it)
//   FAKE_TTY=/dev/ttysN  a new session with that terminal as its controlling one
//   FAKE_CWD=dir         work there
//   FAKE_OPEN=file       keep that file open (Codex keeps its conversation's open)
//   FAKE_LIFE_MS=n       exit after n ms (default 10 minutes: a test that dies leaves nothing for long)
//   FAKE_CHILD=path      start a child running `path`, with FAKE_CHILD_ARGV (argv, \x1f-separated)
//                        and the CHILD_* variables as its FAKE_* ones (CHILD_CHILD_* for its child…)
//   FAKE_EXEC=path       then become `path` with FAKE_EXEC_ARGV (a Node program, say)
//
// With none of these it exits at once: dinod runs a `tmux` it finds running to ask it things.
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

static char **split(const char *s) {
    char *copy = strdup(s);
    int n = 1;
    for (char *p = copy; *p; p++) n += *p == '\x1f';
    char **out = calloc(n + 1, sizeof(char *));
    int i = 0;
    out[i++] = copy;
    for (char *p = copy; *p; p++)
        if (*p == '\x1f') { *p = 0; out[i++] = p + 1; }
    return out;
}

// The environment without this process's FAKE_* variables; CHILD_x becomes x when `child`.
static char **env_for(int child) {
    int n = 0;
    while (environ[n]) n++;
    char **out = calloc(n + 1, sizeof(char *));
    int j = 0;
    for (int i = 0; i < n; i++) {
        const char *e = environ[i];
        if (strncmp(e, "FAKE_", 5) == 0) continue;
        if (strncmp(e, "CHILD_", 6) == 0) {
            if (child) out[j++] = (char *)e + 6;
            continue;
        }
        out[j++] = (char *)e;
    }
    return out;
}

int main(void) {
    const char *v;
    // Run by anyone but a test (a dinod asking what it thinks is tmux): nothing to do here.
    int told = 0;
    for (char **e = environ; *e; e++) told |= strncmp(*e, "FAKE_", 5) == 0;
    if (!told) return 1;
    if ((v = getenv("FAKE_DETACH")) && *v) {
        pid_t pid = fork();
        if (pid < 0) return 1;
        if (pid > 0) {
            printf("%d\n", pid);
            fflush(stdout);
            _exit(0);
        }
        setsid();
        int null = open("/dev/null", O_RDWR);
        dup2(null, 0), dup2(null, 1), dup2(null, 2);
        if (null > 2) close(null);
    }
    if ((v = getenv("FAKE_TTY")) && *v) {
        setsid();
        int fd = open(v, O_RDWR);
        if (fd < 0) return 2;
        ioctl(fd, TIOCSCTTY, 0);
        dup2(fd, 0), dup2(fd, 1), dup2(fd, 2);
        if (fd > 2) close(fd);
    }
    if ((v = getenv("FAKE_CWD")) && *v && chdir(v) != 0) return 3;
    if ((v = getenv("FAKE_OPEN")) && *v && open(v, O_RDWR | O_CREAT | O_APPEND, 0644) < 0) return 4;
    long life = (v = getenv("FAKE_LIFE_MS")) ? atol(v) : 600000;
    if ((v = getenv("FAKE_CHILD")) && *v) {
        const char *argv = getenv("FAKE_CHILD_ARGV");
        pid_t pid = fork();
        if (pid == 0) {
            execve(v, split(argv ? argv : v), env_for(1));
            _exit(127);
        }
    }
    if ((v = getenv("FAKE_EXEC")) && *v) {
        const char *argv = getenv("FAKE_EXEC_ARGV");
        execve(v, split(argv ? argv : v), env_for(0));
        return 127;
    }
    struct timespec ts = {life / 1000, (life % 1000) * 1000000};
    while (nanosleep(&ts, &ts) != 0) {}
    return 0;
}

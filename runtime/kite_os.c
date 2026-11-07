// ------------------------------------------------------------------ io, os
//
// Files, directories, the environment and processes. Everything that can block
// gives its thread's CPU slot to another worker first (kite_blocking_enter), so
// other goroutines keep running. Errors are returned as errno values (negative
// where a result is returned otherwise); std/io.kite turns them into io.Error.

#include <dirent.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <poll.h>
#include <spawn.h>
#include <fcntl.h>

extern char **environ;

// io.ErrorKind, in declaration order
int64_t kite_io___kind(int64_t code) {
    switch ((int)code) {
    case ENOENT: return 1;
    case EACCES: case EPERM: return 2;
    case EEXIST: return 3;
    case ENOTDIR: return 4;
    case EISDIR: return 5;
    case ENOTEMPTY: return 6;
    case EINVAL: case ENAMETOOLONG: return 7;
    case EPIPE: return 9;
    case ECONNREFUSED: return 10;
    case ECONNRESET: return 11;
    case EADDRINUSE: return 12;
    case ETIMEDOUT: return 13;
    case ECANCELED: return 14;
    case EINTR: return 15;
    default: return 0;
    }
}

kite_string kite_io___strerror(int64_t code) {
    kite_string s = { NULL, 0, 0 };
    kite_fmt_cstr(&s, code == ECANCELED ? "cancelled" : strerror((int)code));
    return s;
}

int64_t kite_os___open(kite_str path, int64_t mode) {
    if (has_nul(path)) return -EINVAL;
    char *p = cpath(path);
    int flags = O_CLOEXEC;
    switch (mode) {
    case 0: flags |= O_RDONLY; break;
    case 1: flags |= O_WRONLY | O_CREAT | O_TRUNC; break;
    case 2: flags |= O_WRONLY | O_CREAT | O_APPEND; break;
    default: flags |= O_RDWR | O_CREAT; break;
    }
    kite_blocking_enter();
    int fd = open(p, flags, 0666);
    int e = errno;
    kite_ffi_exit();
    free(p);
    return fd < 0 ? -(int64_t)e : fd;
}

int64_t kite_os___read(int64_t fd, kite_vec *buf, int64_t max) {
    if (max <= 0) return 0;
    kite_vec_reserve(buf, max, 1);
    kite_blocking_enter();
    ssize_t n;
    do {
        n = read((int)fd, (uint8_t *)buf->ptr + buf->len, (size_t)max);
    } while (n < 0 && errno == EINTR);
    int e = errno;
    kite_ffi_exit();
    if (n < 0) return -(int64_t)e;
    buf->len += n;
    return n;
}

int64_t kite_os___write(int64_t fd, kite_slice data) {
    // the standard streams go through the same buffer as print, in order
    if (fd == 1 || fd == 2) {
        kite_ffi_enter();
        pthread_mutex_lock(&g_out_lock);
        FILE *f = fd == 2 ? stderr : stdout;
        if (fd == 2) fflush(stdout);
        size_t n = fwrite(data.ptr, 1, (size_t)data.len, f);
        int bad = n != (size_t)data.len;
        if (fd == 2) fflush(stderr);
        pthread_mutex_unlock(&g_out_lock);
        kite_ffi_exit();
        return bad ? -(int64_t)EIO : 0;
    }
    kite_blocking_enter();
    const uint8_t *p = data.ptr;
    int64_t left = data.len;
    int e = 0;
    while (left > 0) {
        ssize_t n = write((int)fd, p, (size_t)left);
        if (n < 0) {
            if (errno == EINTR) continue;
            e = errno;
            break;
        }
        p += n;
        left -= n;
    }
    kite_ffi_exit();
    return -(int64_t)e;
}

void kite_os___close(int64_t fd) {
    if (fd > 2) close((int)fd);
}

void kite_os___flush(int64_t fd) {
    (void)fd;
    pthread_mutex_lock(&g_out_lock);
    fflush(stdout);
    fflush(stderr);
    pthread_mutex_unlock(&g_out_lock);
}

// kind: 0 file, 1 directory, 2 symbolic link, 3 other
int64_t kite_os___stat(kite_str path, bool follow, int64_t *size, int64_t *mtime, int64_t *kind, int64_t *mode) {
    if (has_nul(path)) return EINVAL;
    char *p = cpath(path);
    struct stat st;
    kite_blocking_enter();
    int r = follow ? stat(p, &st) : lstat(p, &st);
    int e = errno;
    kite_ffi_exit();
    free(p);
    if (r != 0) return e;
    *size = (int64_t)st.st_size;
#if defined(__APPLE__)
    *mtime = (int64_t)st.st_mtimespec.tv_sec * 1000000000 + st.st_mtimespec.tv_nsec;
#else
    *mtime = (int64_t)st.st_mtim.tv_sec * 1000000000 + st.st_mtim.tv_nsec;
#endif
    *kind = S_ISREG(st.st_mode) ? 0 : S_ISDIR(st.st_mode) ? 1 : S_ISLNK(st.st_mode) ? 2 : 3;
    *mode = (int64_t)(st.st_mode & 07777);
    return 0;
}

static int cmp_names(const void *a, const void *b) {
    const kite_string *x = a, *y = b;
    int64_t n = x->len < y->len ? x->len : y->len;
    int c = memcmp(x->ptr, y->ptr, (size_t)n);
    return c ? c : (x->len < y->len ? -1 : x->len > y->len);
}

// The names in a directory, sorted, without "." and "..".
int64_t kite_os___readDir(kite_str path, kite_vec *names) {
    if (has_nul(path)) return EINVAL;
    char *p = cpath(path);
    kite_blocking_enter();
    DIR *d = opendir(p);
    int e = errno;
    if (d) {
        struct dirent *ent;
        while ((ent = readdir(d)) != NULL) {
            if (!strcmp(ent->d_name, ".") || !strcmp(ent->d_name, "..")) continue;
            kite_str s = { (const uint8_t *)ent->d_name, (int64_t)strlen(ent->d_name) };
            vec_push_string(names, s);
        }
        closedir(d);
        e = 0;
    }
    kite_ffi_exit();
    free(p);
    if (names->len > 1) qsort(names->ptr, (size_t)names->len, sizeof(kite_string), cmp_names);
    return e;
}

int64_t kite_os___mkdir(kite_str path) {
    if (has_nul(path)) return EINVAL;
    char *p = cpath(path);
    kite_blocking_enter();
    int r = mkdir(p, 0777);
    int e = errno;
    kite_ffi_exit();
    free(p);
    return r ? e : 0;
}

// A file, or an empty directory.
int64_t kite_os___remove(kite_str path) {
    if (has_nul(path)) return EINVAL;
    char *p = cpath(path);
    kite_blocking_enter();
    int r = unlink(p);
    int e = errno;
    if (r != 0 && (e == EISDIR || e == EPERM)) {
        struct stat st;
        if (lstat(p, &st) == 0 && S_ISDIR(st.st_mode)) {
            r = rmdir(p);
            e = errno;
        }
    }
    kite_ffi_exit();
    free(p);
    return r ? e : 0;
}

int64_t kite_os___rename(kite_str from, kite_str to) {
    if (has_nul(from) || has_nul(to)) return EINVAL;
    char *a = cpath(from), *b = cpath(to);
    kite_blocking_enter();
    int r = rename(a, b);
    int e = errno;
    kite_ffi_exit();
    free(a);
    free(b);
    return r ? e : 0;
}

int64_t kite_os___cwd(kite_string *out) {
    char buf[4096];
    if (!getcwd(buf, sizeof buf)) return errno;
    kite_fmt_cstr(out, buf);
    return 0;
}

int64_t kite_os___chdir(kite_str path) {
    if (has_nul(path)) return EINVAL;
    char *p = cpath(path);
    int r = chdir(p);
    int e = errno;
    free(p);
    return r ? e : 0;
}

static pthread_mutex_t g_env_lock = PTHREAD_MUTEX_INITIALIZER;

int64_t kite_os___setenv(kite_str name, kite_str value, bool unset) {
    if (has_nul(name) || has_nul(value)) return EINVAL;
    char *n = cpath(name), *v = cpath(value);
    pthread_mutex_lock(&g_env_lock);
    int r = unset ? unsetenv(n) : setenv(n, v, 1);
    int e = errno;
    pthread_mutex_unlock(&g_env_lock);
    free(n);
    free(v);
    return r ? e : 0;
}

void kite_os___tempDir(kite_string *out) {
    const char *t = getenv("TMPDIR");
    if (!t || !*t) t = "/tmp";
    size_t n = strlen(t);
    while (n > 1 && t[n - 1] == '/') n--;
    kite_str s = { (const uint8_t *)t, (int64_t)n };
    kite_string_push(out, s);
}

// Runs `prog` with `args`. With `capture`, the child reads `input` and its
// output goes to `out` / `errout`; otherwise it shares this process's streams.
// `status` is the exit code, or 128 + the signal that ended it. Returns an
// errno when the program can't be started.
static pthread_mutex_t g_spawn_lock = PTHREAD_MUTEX_INITIALIZER;

static void drain(int fd, kite_vec *v, bool *open_) {
    kite_vec_reserve(v, 65536, 1);
    ssize_t n = read(fd, (uint8_t *)v->ptr + v->len, 65536);
    if (n > 0) v->len += n;
    else if (n == 0 || errno != EINTR) *open_ = false;
}

int64_t kite_os___run(kite_str prog, kite_slice args, kite_slice input, bool capture, kite_vec *out, kite_vec *errout, int64_t *status) {
    if (has_nul(prog)) return EINVAL;
    for (int64_t i = 0; i < args.len; i++)
        if (has_nul(kite_string_view(&((kite_string *)args.ptr)[i]))) return EINVAL;
    int64_t argc = args.len;
    char **argv = kite_alloc(sizeof(char *) * (size_t)(argc + 2));
    argv[0] = cpath(prog);
    for (int64_t i = 0; i < argc; i++) argv[i + 1] = cpath(kite_string_view(&((kite_string *)args.ptr)[i]));
    argv[argc + 1] = NULL;
    int pin[2] = { -1, -1 }, pout[2] = { -1, -1 }, perr[2] = { -1, -1 };
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    kite_blocking_enter();
    pthread_mutex_lock(&g_spawn_lock);
    int e = 0;
    if (capture) {
        if (pipe(pin) || pipe(pout) || pipe(perr)) e = errno;
        int fds[6] = { pin[0], pin[1], pout[0], pout[1], perr[0], perr[1] };
        for (int i = 0; i < 6 && !e; i++) fcntl(fds[i], F_SETFD, FD_CLOEXEC);
        if (!e) {
            posix_spawn_file_actions_adddup2(&fa, pin[0], 0);
            posix_spawn_file_actions_adddup2(&fa, pout[1], 1);
            posix_spawn_file_actions_adddup2(&fa, perr[1], 2);
        }
    } else {
        // the child writes to the same streams: what we printed comes first
        fflush(stdout);
        fflush(stderr);
    }
    pid_t pid = -1;
    if (!e) e = posix_spawnp(&pid, argv[0], &fa, NULL, argv, environ);
    pthread_mutex_unlock(&g_spawn_lock);
    posix_spawn_file_actions_destroy(&fa);
    if (capture) {
        if (pin[0] >= 0) close(pin[0]);
        if (pout[1] >= 0) close(pout[1]);
        if (perr[1] >= 0) close(perr[1]);
    }
    if (!e && capture) {
        // feed stdin and read both outputs at once, so that no pipe fills up
        const uint8_t *ip = input.ptr;
        int64_t ileft = input.len;
        bool oo = true, eo = true;
        if (ileft == 0) {
            close(pin[1]);
            pin[1] = -1;
        }
        sigset_t pipe_set, old;
        sigemptyset(&pipe_set);
        sigaddset(&pipe_set, SIGPIPE);
        pthread_sigmask(SIG_BLOCK, &pipe_set, &old);
        while (oo || eo) {
            struct pollfd pf[3];
            int np = 0, io = -1, ie = -1, ii = -1;
            if (oo) { pf[np].fd = pout[0]; pf[np].events = POLLIN; io = np++; }
            if (eo) { pf[np].fd = perr[0]; pf[np].events = POLLIN; ie = np++; }
            if (pin[1] >= 0) { pf[np].fd = pin[1]; pf[np].events = POLLOUT; ii = np++; }
            if (poll(pf, (nfds_t)np, -1) < 0) {
                if (errno == EINTR) continue;
                break;
            }
            if (io >= 0 && pf[io].revents) drain(pout[0], out, &oo);
            if (ie >= 0 && pf[ie].revents) drain(perr[0], errout, &eo);
            if (ii >= 0 && pf[ii].revents) {
                ssize_t n = write(pin[1], ip, (size_t)(ileft > 65536 ? 65536 : ileft));
                if (n > 0) {
                    ip += n;
                    ileft -= n;
                }
                if (n < 0 && errno == EPIPE) {
                    // the child stopped reading: consume the signal we caused
                    int sig;
                    sigset_t pend;
                    sigpending(&pend);
                    if (sigismember(&pend, SIGPIPE)) sigwait(&pipe_set, &sig);
                    ileft = 0;
                }
                if (ileft == 0 || (n < 0 && errno != EINTR && errno != EAGAIN)) {
                    close(pin[1]);
                    pin[1] = -1;
                }
            }
        }
        pthread_sigmask(SIG_SETMASK, &old, NULL);
    }
    if (capture) {
        if (pin[1] >= 0) close(pin[1]);
        if (pout[0] >= 0) close(pout[0]);
        if (perr[0] >= 0) close(perr[0]);
    }
    if (!e) {
        int st = 0;
        while (waitpid(pid, &st, 0) < 0 && errno == EINTR) {
        }
        *status = WIFEXITED(st) ? WEXITSTATUS(st) : WIFSIGNALED(st) ? 128 + WTERMSIG(st) : 1;
    }
    kite_ffi_exit();
    for (int64_t i = 0; i <= argc; i++) free(argv[i]);
    free(argv);
    return e;
}

// ------------------------------------------------------------------ time (wall clock)

int64_t kite_time___unixNanos(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

// The local time zone's offset from UTC in seconds at `secs` (Unix time).
int64_t kite_time___localOffset(int64_t secs) {
    time_t t = (time_t)secs;
    struct tm tm;
    localtime_r(&t, &tm);
    return (int64_t)tm.tm_gmtoff;
}

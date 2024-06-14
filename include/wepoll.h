#ifndef WEPOLL_H_
#define WEPOLL_H_

#include <stdint.h>
#include <stdbool.h>
#include <time.h>

/* clang-format off */

enum EPOLL_EVENTS {
  EPOLLIN      = (int) (1U << 0),
  EPOLLOUT     = (int) (1U << 1),
  EPOLLHUP     = (int) (1U << 2),
  EPOLLERR     = (int) (1U << 6),
  EPOLLET      = (int) (1U << 8),
  EPOLLONESHOT = (int) (1U << 9)
};

#define EPOLLIN      (1U << 0)
#define EPOLLOUT     (1U << 1)
#define EPOLLHUP     (1U << 2)
#define EPOLLERR     (1U << 6)
#define EPOLLET      (1U << 8)
#define EPOLLONESHOT (1U << 9)

#define EPOLL_CTL_ADD 1
#define EPOLL_CTL_MOD 2
#define EPOLL_CTL_DEL 3

/* clang-format on */

typedef void* HANDLE;
typedef uintptr_t SOCKET;

typedef union epoll_data {
  void* ptr;
  int fd;
  uint32_t u32;
#if UINTPTR_MAX == UINT64_MAX
  uint64_t u64;
#endif
  SOCKET sock; /* Windows specific */
  HANDLE hnd;  /* Windows specific */
} epoll_data_t;

struct epoll_event {
  epoll_data_t data; /* User data variable */
  void* __overlapped;
  size_t __internal;
  uint32_t events;   /* Epoll events and flags */
};

#ifdef __cplusplus
extern "C" {
#endif

HANDLE epoll_create(int size);
HANDLE epoll_create1(int flags);

int epoll_close(HANDLE ephnd);

int epoll_ctl(HANDLE ephnd,
              int op,
              HANDLE handle,
              struct epoll_event* event);

int epoll_wait(HANDLE ephnd,
               struct epoll_event* events,
               int maxevents,
               int timeout);
int epoll_pwait(HANDLE ephnd,
                struct epoll_event* events,
                int maxevents,
                int timeout,
                bool alertable);
int epoll_pwait2(HANDLE ephnd,
                 struct epoll_event* events,
                 int maxevents,
                 struct timespec* timeout,
                 bool alertable);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* WEPOLL_H_ */
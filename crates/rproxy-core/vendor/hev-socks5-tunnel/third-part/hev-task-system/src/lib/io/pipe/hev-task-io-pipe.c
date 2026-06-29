/*
 ============================================================================
 Name        : hev-task-io-pipe.c
 Author      : Heiher <r@hev.cc>
 Copyright   : Copyright (c) 2018 everyone.
 Description : Task pipe I/O operations
 ============================================================================
 */

#include <unistd.h>
#if defined(_WIN32) || defined(_WIN64)
#include <fcntl.h>
#include <stdlib.h>
#define pipe(fds) _pipe ((fds), 4096, _O_BINARY)
#endif
#include <sys/ioctl.h>

#include "lib/misc/hev-compiler.h"

#include "hev-task-io-pipe.h"

EXPORT_SYMBOL int
hev_task_io_pipe_pipe (int pipefd[2])
{
    int nonblock = 1;

    if (0 > pipe (pipefd))
        return -1;

    if (0 > ioctl (pipefd[0], FIONBIO, (char *)&nonblock)) {
        close (pipefd[0]);
        close (pipefd[1]);
        return -2;
    }

    if (0 > ioctl (pipefd[1], FIONBIO, (char *)&nonblock)) {
        close (pipefd[0]);
        close (pipefd[1]);
        return -3;
    }

    return 0;
}

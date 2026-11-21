#include "novella_rt.h"

#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void* (*global_malloc)(size_t size) = malloc;
void (*global_free)(void* ptr) = free;

/* weak 持有独立控制块；最后强引用释放载荷后，控制块仍允许安全判活。 */
struct novella_rt_weak {
    _Atomic size_t strong;
    /* 存在强引用时保留一份隐式 weak，防止析构与最后一个 weak 释放竞争。 */
    _Atomic size_t weak;
    void* object;
    void* allocation;
    novella_rt_destructor destroy;
};

/* 隐藏对象头与用户载荷分离，用户类型不需要暴露控制块或分配器字段。 */
typedef union {
    novella_rt_weak* control;
    max_align_t alignment;
} ObjectHeader;

static _Noreturn void fail(const char* message) {
    fprintf(stderr, "novella runtime: %s\n", message);
    abort();
}

static ObjectHeader* header_of(void* object) {
    return (ObjectHeader*)((unsigned char*)object - sizeof(ObjectHeader));
}

static void increment(_Atomic size_t* count, const char* message) {
    size_t previous = atomic_load_explicit(count, memory_order_relaxed);
    do {
        if (previous == 0 || previous == SIZE_MAX) {
            fail(message);
        }
    } while (!atomic_compare_exchange_weak_explicit(
        count, &previous, previous + 1, memory_order_relaxed,
        memory_order_relaxed));
}

void* novella_rt_alloc(size_t size,
                       size_t alignment,
                       novella_rt_destructor destroy) {
    if (alignment == 0 || (alignment & (alignment - 1)) != 0) {
        fail("对象对齐必须是非零的 2 的幂");
    }
    if (alignment < _Alignof(ObjectHeader)) {
        alignment = _Alignof(ObjectHeader);
    }
    /* 空类型也分配独立身份；先检查加法，避免溢出后分配过小的缓冲区。 */
    size_t payload_size = size == 0 ? 1 : size;
    size_t padding = alignment - 1;
    if (padding > SIZE_MAX - sizeof(ObjectHeader) ||
        payload_size > SIZE_MAX - sizeof(ObjectHeader) - padding) {
        fail("对象布局超出地址空间");
    }
    novella_rt_weak* control = novella_rt_malloc(sizeof(*control));
    if (control == NULL) {
        fail("对象控制块分配失败");
    }
    void* allocation =
        novella_rt_malloc(sizeof(ObjectHeader) + padding + payload_size);
    if (allocation == NULL) {
        novella_rt_free(control);
        fail("对象内存分配失败");
    }
    uintptr_t start = (uintptr_t)allocation + sizeof(ObjectHeader);
    void* object = (void*)((start + padding) & ~(uintptr_t)padding);
    header_of(object)->control = control;
    control->object = object;
    control->allocation = allocation;
    control->destroy = destroy;
    atomic_init(&control->strong, 1);
    atomic_init(&control->weak, 1);
    memset(object, 0, payload_size);
    return object;
}

void novella_rt_retain(void* object) {
    if (object != NULL) {
        increment(&header_of(object)->control->strong,
                  "对象引用计数无效或溢出");
    }
}

void novella_rt_weak_retain(novella_rt_weak* weak) {
    if (weak != NULL) {
        increment(&weak->weak, "弱引用计数无效或溢出");
    }
}

void novella_rt_weak_release(novella_rt_weak* weak) {
    if (weak == NULL) {
        return;
    }
    size_t previous =
        atomic_fetch_sub_explicit(&weak->weak, 1, memory_order_release);
    if (previous == 0) {
        fail("不能重复释放弱引用");
    }
    if (previous == 1) {
        atomic_thread_fence(memory_order_acquire);
        novella_rt_free(weak);
    }
}

novella_rt_weak* novella_rt_weak_create(void* object) {
    if (object == NULL) {
        return NULL;
    }
    /* 调用方必须持有强引用，因此此处隐式 weak 尚在、对象头也仍有效。 */
    novella_rt_weak* weak = header_of(object)->control;
    novella_rt_weak_retain(weak);
    return weak;
}

void* novella_rt_weak_upgrade(novella_rt_weak* weak) {
    if (weak == NULL) {
        return NULL;
    }
    size_t strong = atomic_load_explicit(&weak->strong, memory_order_relaxed);
    do {
        if (strong == 0) {
            return NULL;
        }
        if (strong == SIZE_MAX) {
            fail("对象引用计数溢出");
        }
        /* 与最后一次强 release 在同一原子计数上竞争，零不可逆。 */
    } while (!atomic_compare_exchange_weak_explicit(
        &weak->strong, &strong, strong + 1, memory_order_acquire,
        memory_order_relaxed));
    return weak->object;
}

void novella_rt_release(void* object) {
    if (object == NULL) {
        return;
    }
    novella_rt_weak* control = header_of(object)->control;
    size_t previous =
        atomic_fetch_sub_explicit(&control->strong, 1, memory_order_release);
    if (previous == 0) {
        fail("不能释放正在析构的对象");
    }
    if (previous != 1) {
        return;
    }
    /* 最后的所有者必须观察其他所有者在 release 前完成的资源写入。 */
    atomic_thread_fence(memory_order_acquire);
    if (control->destroy != NULL) {
        control->destroy(object);
    }
    novella_rt_free(control->allocation);
    /* 析构期间 weak upgrade 也只会看到零；最后移除隐式 weak。 */
    novella_rt_weak_release(control);
}

void* novella_rt_malloc(size_t size) {
    if (global_malloc == NULL) {
        fail("内存分配器未设置");
    }
    return global_malloc(size);
}

void novella_rt_free(void* ptr) {
    if (ptr == NULL) {
        return;
    }
    if (global_free == NULL) {
        fail("内存释放器未设置");
    }
    global_free(ptr);
}

// 设置全局内存分配器
void novella_rt_set_allocator(void* (*malloc)(size_t size),
                              void (*free)(void* ptr)) {
    global_malloc = malloc;
    global_free = free;
}
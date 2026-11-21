#include "novella_rt.h"

#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* 隐藏对象头与用户载荷分离，用户类型不需要暴露引用计数或分配器字段。 */
typedef union {
    struct {
        void *allocation;
        _Atomic size_t strong;
        novella_rt_destructor destroy;
    } fields;
    max_align_t alignment;
} ObjectHeader;

static _Noreturn void fail(const char *message) {
    fprintf(stderr, "novella runtime: %s\n", message);
    abort();
}

static ObjectHeader *header_of(void *object) {
    return (ObjectHeader *)((unsigned char *)object - sizeof(ObjectHeader));
}

void *novella_rt_alloc(size_t size, size_t alignment, novella_rt_destructor destroy) {
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
    void *allocation = malloc(sizeof(ObjectHeader) + padding + payload_size);
    if (allocation == NULL) {
        fail("对象内存分配失败");
    }
    uintptr_t start = (uintptr_t)allocation + sizeof(ObjectHeader);
    void *object = (void *)((start + padding) & ~(uintptr_t)padding);
    ObjectHeader *header = header_of(object);
    header->fields.allocation = allocation;
    header->fields.destroy = destroy;
    atomic_init(&header->fields.strong, 1);
    memset(object, 0, payload_size);
    return object;
}

void novella_rt_retain(void *object) {
    if (object == NULL) {
        return;
    }
    ObjectHeader *header = header_of(object);
    size_t strong = atomic_load_explicit(&header->fields.strong, memory_order_relaxed);
    do {
        if (strong == 0 || strong == SIZE_MAX) {
            fail("对象引用计数无效或溢出");
        }
    } while (!atomic_compare_exchange_weak_explicit(
        &header->fields.strong, &strong, strong + 1,
        memory_order_relaxed, memory_order_relaxed));
}

void novella_rt_release(void *object) {
    if (object == NULL) {
        return;
    }
    ObjectHeader *header = header_of(object);
    size_t previous = atomic_fetch_sub_explicit(&header->fields.strong, 1, memory_order_release);
    if (previous == 0) {
        fail("不能释放正在析构的对象");
    }
    if (previous != 1) {
        return;
    }
    /* 最后的所有者必须观察其他所有者在 release 前完成的资源写入。 */
    atomic_thread_fence(memory_order_acquire);
    void *allocation = header->fields.allocation;
    if (header->fields.destroy != NULL) {
        header->fields.destroy(object);
    }
    free(allocation);
}

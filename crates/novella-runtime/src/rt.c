#include "novella_rt.h"

#include <stdbool.h>
#include <stdatomic.h>
#include <stdint.h>

/* 配置字段只在锁内访问；固定后不再写入，回调在锁外执行以允许重入。 */
static atomic_flag allocator_lock = ATOMIC_FLAG_INIT;
static bool allocator_fixed;
static void* (*configured_alloc)(size_t);
static void (*configured_free)(void*);

/* 首次使用后在锁内解析一次最终入口，之后每次分配只做一次 acquire 读取。
 * 两个入口在同一临界区内成对发布：看到 resolved_alloc 非空即意味着 resolved_free
 * 也已发布（见 resolve_allocator 的说明）。 */
static _Atomic(void* (*)(size_t)) resolved_alloc;
static _Atomic(void (*)(void*)) resolved_free;

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

static _Noreturn void fail(unsigned code) {
    /* 经 volatile 指针调用，避免 noreturn 声明让优化器删除返回后的 trap。 */
    void (*volatile panic_hook)(unsigned) = novella_platform_panic;
    panic_hook(code);
    __builtin_trap();
}

static void lock_allocator(void) {
    while (atomic_flag_test_and_set_explicit(&allocator_lock, memory_order_acquire)) {
        /* 仅保护短暂配置，不在持锁期间调用平台或用户代码。 */
    }
}

static void unlock_allocator(void) {
    atomic_flag_clear_explicit(&allocator_lock, memory_order_release);
}

static ObjectHeader* header_of(void* object) {
    return (ObjectHeader*)((unsigned char*)object - sizeof(ObjectHeader));
}

static void increment(_Atomic size_t* count, unsigned code) {
    size_t previous = atomic_load_explicit(count, memory_order_relaxed);
    do {
        if (previous == 0 || previous == SIZE_MAX) {
            fail(code);
        }
    } while (!atomic_compare_exchange_weak_explicit(
        count, &previous, previous + 1, memory_order_relaxed,
        memory_order_relaxed));
}

/* 载荷清零：按字宽写，而不是逐字节 store。结构同 musl memset 的
 * 「先对齐、再按字宽写主体、最后补尾」，只是对象已由 novella_rt_alloc 保证
 * 至少 8 字节对齐，对齐循环通常零次。
 * rt.c 以 -fno-builtin 构建，循环不会被折叠成 memset 调用；memory.c 的 memset
 * 属于独立归档，managed 归档不能依赖它。 */
static void zero_payload(void* object, size_t size) {
    unsigned char* bytes = object;
    while (size != 0 && ((uintptr_t)bytes & (sizeof(size_t) - 1)) != 0) {
        *bytes++ = 0;
        --size;
    }
    size_t* words = (size_t*)bytes;
    size_t count = size / sizeof(size_t);
    for (size_t index = 0; index < count; ++index) {
        words[index] = 0;
    }
    bytes = (unsigned char*)(words + count);
    for (size_t index = 0; index < size % sizeof(size_t); ++index) {
        bytes[index] = 0;
    }
}

void* novella_rt_alloc(size_t size,
                       size_t alignment,
                       novella_rt_destructor destroy) {
    if (alignment == 0 || (alignment & (alignment - 1)) != 0) {
        fail(NOVELLA_RT_INVALID_ALIGNMENT);
    }
    if (alignment < _Alignof(ObjectHeader)) {
        alignment = _Alignof(ObjectHeader);
    }
    /* 空类型也分配独立身份；先检查加法，避免溢出后分配过小的缓冲区。 */
    size_t payload_size = size == 0 ? 1 : size;
    size_t padding = alignment - 1;
    if (padding > SIZE_MAX - sizeof(ObjectHeader) ||
        payload_size > SIZE_MAX - sizeof(ObjectHeader) - padding) {
        fail(NOVELLA_RT_LAYOUT_OVERFLOW);
    }
    novella_rt_weak* control = novella_rt_malloc(sizeof(*control));
    if (control == NULL) {
        fail(NOVELLA_RT_CONTROL_ALLOCATION_FAILED);
    }
    void* allocation =
        novella_rt_malloc(sizeof(ObjectHeader) + padding + payload_size);
    if (allocation == NULL) {
        novella_rt_free(control);
        fail(NOVELLA_RT_OBJECT_ALLOCATION_FAILED);
    }
    uintptr_t start = (uintptr_t)allocation + sizeof(ObjectHeader);
    void* object = (void*)((start + padding) & ~(uintptr_t)padding);
    header_of(object)->control = control;
    control->object = object;
    control->allocation = allocation;
    control->destroy = destroy;
    atomic_init(&control->strong, 1);
    atomic_init(&control->weak, 1);
    zero_payload(object, payload_size);
    return object;
}

void novella_rt_retain(void* object) {
    if (object != NULL) {
        increment(&header_of(object)->control->strong,
                  NOVELLA_RT_INVALID_STRONG_COUNT);
    }
}

void novella_rt_weak_retain(novella_rt_weak* weak) {
    if (weak != NULL) {
        increment(&weak->weak, NOVELLA_RT_INVALID_WEAK_COUNT);
    }
}

void novella_rt_weak_release(novella_rt_weak* weak) {
    if (weak == NULL) {
        return;
    }
    size_t previous =
        atomic_fetch_sub_explicit(&weak->weak, 1, memory_order_release);
    if (previous == 0) {
        fail(NOVELLA_RT_DOUBLE_WEAK_RELEASE);
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
            fail(NOVELLA_RT_INVALID_STRONG_COUNT);
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
        fail(NOVELLA_RT_DOUBLE_STRONG_RELEASE);
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

/* 幂等地解析最终入口：配置过的 setter 优先，否则回落到平台 hooks。
 * 两个指针在同一临界区内成对发布，空指针是「尚未解析」的唯一标记；进入临界区后
 * 「resolved_alloc 非空」必然蕴含「resolved_free 也非空」，因此首轮之后就无需再加锁。 */
static void resolve_allocator(void) {
    lock_allocator();
    allocator_fixed = true;
    if (atomic_load_explicit(&resolved_alloc, memory_order_relaxed) == NULL) {
        atomic_store_explicit(
            &resolved_alloc,
            configured_alloc != NULL ? configured_alloc : novella_platform_alloc,
            memory_order_release);
        atomic_store_explicit(
            &resolved_free,
            configured_free != NULL ? configured_free : novella_platform_free,
            memory_order_release);
    }
    unlock_allocator();
}

void* novella_rt_malloc(size_t size) {
    void* (*allocate)(size_t) =
        atomic_load_explicit(&resolved_alloc, memory_order_acquire);
    if (allocate == NULL) {
        resolve_allocator();
        allocate = atomic_load_explicit(&resolved_alloc, memory_order_acquire);
    }
    /* 回调在锁外执行，因此可以在实现内部再次进入本文件。 */
    return allocate(size);
}

void novella_rt_free(void* ptr) {
    if (ptr == NULL) {
        return;
    }
    void (*deallocate)(void*) =
        atomic_load_explicit(&resolved_free, memory_order_acquire);
    if (deallocate == NULL) {
        resolve_allocator();
        deallocate = atomic_load_explicit(&resolved_free, memory_order_acquire);
    }
    deallocate(ptr);
}

void novella_rt_set_allocator(void* (*allocate)(size_t size),
                              void (*deallocate)(void* ptr)) {
    if (allocate == NULL || deallocate == NULL) {
        fail(NOVELLA_RT_INVALID_ALLOCATOR);
    }
    lock_allocator();
    if (allocator_fixed) {
        unlock_allocator();
        fail(NOVELLA_RT_ALLOCATOR_FIXED);
    }
    configured_alloc = allocate;
    configured_free = deallocate;
    unlock_allocator();
}

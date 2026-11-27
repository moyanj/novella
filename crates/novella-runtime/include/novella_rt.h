#ifndef NOVELLA_RT_H
#define NOVELLA_RT_H

#include <stddef.h>

/* 平台层必须提供这三个 hook；hosted provider 使用 libc，裸机由使用者实现。
 * alloc 返回适合基本 C 类型对齐的内存，失败返回空；free 接受原 alloc 返回值。
 * panic 不得返回；runtime 仍在 hook 意外返回时执行 trap。 */
void* novella_platform_alloc(size_t size);
void novella_platform_free(void* ptr);
_Noreturn void novella_platform_panic(unsigned code);

/* 错误码是平台 panic ABI 的稳定值，平台可以记录、停机或转交内核。 */
typedef enum {
    NOVELLA_RT_INVALID_ALIGNMENT = 1,
    NOVELLA_RT_LAYOUT_OVERFLOW = 2,
    NOVELLA_RT_CONTROL_ALLOCATION_FAILED = 3,
    NOVELLA_RT_OBJECT_ALLOCATION_FAILED = 4,
    NOVELLA_RT_INVALID_STRONG_COUNT = 5,
    NOVELLA_RT_INVALID_WEAK_COUNT = 6,
    NOVELLA_RT_DOUBLE_WEAK_RELEASE = 7,
    NOVELLA_RT_DOUBLE_STRONG_RELEASE = 8,
    NOVELLA_RT_INVALID_ALLOCATOR = 9,
    NOVELLA_RT_ALLOCATOR_FIXED = 10
} novella_rt_error;

/* 析构回调只清理对象拥有的资源；对象本体由 runtime 在回调返回后释放。 */
typedef void (*novella_rt_destructor)(void* object);

/* 返回零初始化的对象载荷，初始强引用计数为 1。alignment 必须是非零的 2 的幂。
 * 无效布局、长度溢出和分配失败交由平台 panic 终止执行。destroy 可为空。 */
void* novella_rt_alloc(size_t size,
                       size_t alignment,
                       novella_rt_destructor destroy);

/* 每份共享强引用配对一次 retain/release；空指针操作为空操作。
 * 最后一个 release 同步调用析构并释放对象；不得复活或重复释放正在析构的对象。
 * 原子计数允许不同强引用跨线程释放，不使对象字段的并发读写自动安全。 */
void novella_rt_retain(void* object);
void novella_rt_release(void* object);

/* weak 字段保存独立控制块指针，不是对象载荷指针；零初始化表示空 weak。
 * 目标析构后字段中的控制块指针仍有效，但 upgrade 永远返回空。
 * 控制块在最后一份 weak 释放后回收，不延长对象载荷或其资源的生命周期。 */
typedef struct novella_rt_weak novella_rt_weak;

/* create 要求调用者持有 object 的强引用，返回一份拥有的 weak。
 * 复制控制块指针必须 retain，覆盖/销毁字段必须 release；三者空指针均无操作。
 * 同一 weak 槽位的并发读写须由调用者同步，计数原子化不保护槽位。 */
novella_rt_weak* novella_rt_weak_create(void* object);
void novella_rt_weak_retain(novella_rt_weak* weak);
void novella_rt_weak_release(novella_rt_weak* weak);

/* 调用方须持有有效 weak。成功时原子地取得一份强引用并返回载荷，须 release；
 * 目标已开始析构或 weak 为空时返回空。不允许强计数从零复活，
 * 因而可与最后一份强引用的释放并发；析构回调内升级自身也返回空。 */
void* novella_rt_weak_upgrade(novella_rt_weak* weak);

/* raw 分配保留分配失败返回空的行为；所有对象和控制块统一经过此入口。
 * 第一次 malloc 或非空 free 永久固定分配器，之后 setter 会 panic。
 * setter 与首次使用通过原子锁串行化，并在锁内解析出最终入口；此后的每次
 * malloc/free 只做一次 acquire 读取，不再取锁。两个回调必须同时非空，且必须线程安全。
 * 回调在锁外执行，可以重入 runtime；运行期不能替换，保证 free 始终配对同一分配器。
 * 未调用 setter 时使用平台 hooks。free(NULL) 无操作，也不会固定分配器。 */
void* novella_rt_malloc(size_t size);
void novella_rt_free(void* ptr);
void novella_rt_set_allocator(void* (*malloc)(size_t size),
                              void (*free)(void* ptr));
#endif

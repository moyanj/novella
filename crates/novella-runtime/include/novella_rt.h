#ifndef NOVELLA_RT_H
#define NOVELLA_RT_H

#include <stddef.h>

/* 析构回调只清理对象拥有的资源；对象本体由 runtime 在回调返回后释放。 */
typedef void (*novella_rt_destructor)(void *object);

/* 返回零初始化的对象载荷，初始强引用计数为 1。alignment 必须是非零的 2 的幂。
 * 无效布局、长度溢出和分配失败会终止进程。destroy 可为空。 */
void *novella_rt_alloc(size_t size, size_t alignment, novella_rt_destructor destroy);

/* 每份共享强引用配对一次 retain/release；空指针操作为空操作。
 * 最后一个 release 同步调用析构并释放对象；不得复活或重复释放正在析构的对象。
 * 原子计数允许不同强引用跨线程释放，不使对象字段的并发读写自动安全。 */
void novella_rt_retain(void *object);
void novella_rt_release(void *object);

/* weak 字段直接保存对象载荷指针，不调用 retain/release，也不分配控制块。
 * 对象销毁后不会自动清空 weak 字段；调用者必须另行保证对象仍然存活，
 * 才能通过该指针访问对象或取得强引用。仅检查非空不能证明对象仍然有效。
 * 释放 weak 字段所属对象时，不得 release 该字段指向的对象。 */

#endif

#include "novella_rt.h"

#include <assert.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>

/* 验证析构期间不能复活，并允许对象在回调中释放指向自身的 weak。 */
typedef struct {
    int value;
    novella_rt_weak *self;
} Payload;

static _Atomic unsigned destroyed;

static void destroy(void *object) {
    Payload *payload = object;
    assert(payload->value == 42);
    assert(novella_rt_weak_upgrade(payload->self) == NULL);
    novella_rt_weak_release(payload->self);
    atomic_fetch_add_explicit(&destroyed, 1, memory_order_relaxed);
}

static Payload *make_payload(void) {
    Payload *payload = novella_rt_alloc(sizeof(Payload), _Alignof(Payload), destroy);
    assert(payload->value == 0);
    assert(payload->self == NULL);
    payload->value = 42;
    payload->self = novella_rt_weak_create(payload);
    return payload;
}

typedef struct {
    novella_rt_weak *weak;
    _Atomic unsigned ready;
    _Atomic bool start;
} Race;

static void *upgrade_worker(void *argument) {
    Race *race = argument;
    atomic_fetch_add_explicit(&race->ready, 1, memory_order_release);
    while (!atomic_load_explicit(&race->start, memory_order_acquire)) {
        sched_yield();
    }
    for (unsigned index = 0; index < 1000; ++index) {
        Payload *payload = novella_rt_weak_upgrade(race->weak);
        if (payload != NULL) {
            assert(payload->value == 42);
            novella_rt_release(payload);
        }
    }
    /* 每个线程持有独立 weak，避免把计数安全误当作 weak 槽位安全。 */
    novella_rt_weak_release(race->weak);
    return NULL;
}

int main(void) {
    atomic_init(&destroyed, 0);
    novella_rt_retain(NULL);
    novella_rt_release(NULL);
    novella_rt_weak_retain(NULL);
    novella_rt_weak_release(NULL);
    assert(novella_rt_weak_create(NULL) == NULL);
    assert(novella_rt_weak_upgrade(NULL) == NULL);

    /* 对齐、空对象身份与原有强引用 ABI 保持兼容。 */
    void *empty = novella_rt_alloc(0, 64, NULL);
    assert((uintptr_t)empty % 64 == 0);
    novella_rt_retain(empty);
    novella_rt_release(empty);
    novella_rt_release(empty);

    Payload *payload = make_payload();
    novella_rt_weak *weak = novella_rt_weak_create(payload);
    novella_rt_weak_retain(weak);
    Payload *promoted = novella_rt_weak_upgrade(weak);
    assert(promoted == payload);
    novella_rt_release(payload);
    assert(atomic_load_explicit(&destroyed, memory_order_relaxed) == 0);
    novella_rt_release(promoted);
    assert(atomic_load_explicit(&destroyed, memory_order_relaxed) == 1);
    assert(novella_rt_weak_upgrade(weak) == NULL);
    novella_rt_weak_release(weak);
    assert(novella_rt_weak_upgrade(weak) == NULL);
    novella_rt_weak_release(weak);

    for (unsigned round = 0; round < 64; ++round) {
        payload = make_payload();
        Race race = { .weak = novella_rt_weak_create(payload) };
        atomic_init(&race.ready, 0);
        atomic_init(&race.start, false);
        pthread_t workers[4];
        for (unsigned index = 0; index < 4; ++index) {
            novella_rt_weak_retain(race.weak);
            assert(pthread_create(&workers[index], NULL, upgrade_worker, &race) == 0);
        }
        while (atomic_load_explicit(&race.ready, memory_order_acquire) != 4) {
            sched_yield();
        }
        atomic_store_explicit(&race.start, true, memory_order_release);
        novella_rt_release(payload);
        for (unsigned index = 0; index < 4; ++index) {
            assert(pthread_join(workers[index], NULL) == 0);
        }
        assert(atomic_load_explicit(&destroyed, memory_order_relaxed) == round + 2);
        assert(novella_rt_weak_upgrade(race.weak) == NULL);
        novella_rt_weak_release(race.weak);
    }
    return 0;
}

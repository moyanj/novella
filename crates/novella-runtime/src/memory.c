#include <stddef.h>
#include <stdint.h>

/* 独立 freestanding support，不依赖 ARC 或平台分配器。
 * volatile 字节访问与构建时 -fno-builtin 双重防止循环优化回自身调用。 */
void* memcpy(void* restrict destination, const void* restrict source, size_t size) {
    volatile unsigned char* output = destination;
    const volatile unsigned char* input = source;
    for (size_t index = 0; index < size; ++index) {
        output[index] = input[index];
    }
    return destination;
}

void* memmove(void* destination, const void* source, size_t size) {
    volatile unsigned char* output = destination;
    const volatile unsigned char* input = source;
    /* uintptr_t 比较避免对不属于同一数组的指针做关系比较。 */
    if ((uintptr_t)destination <= (uintptr_t)source) {
        for (size_t index = 0; index < size; ++index) {
            output[index] = input[index];
        }
    } else {
        while (size != 0) {
            --size;
            output[size] = input[size];
        }
    }
    return destination;
}

void* memset(void* destination, int value, size_t size) {
    volatile unsigned char* output = destination;
    for (size_t index = 0; index < size; ++index) {
        output[index] = (unsigned char)value;
    }
    return destination;
}

int memcmp(const void* left, const void* right, size_t size) {
    const volatile unsigned char* first = left;
    const volatile unsigned char* second = right;
    for (size_t index = 0; index < size; ++index) {
        unsigned char a = first[index];
        unsigned char b = second[index];
        if (a != b) {
            return (int)a - (int)b;
        }
    }
    return 0;
}

#include "novella_rt.h"

#include <stdlib.h>

/* libc 依赖仅存在于 hosted provider；裸机归档不包含本对象文件。 */
void* novella_platform_alloc(size_t size) {
    return malloc(size);
}

void novella_platform_free(void* ptr) {
    free(ptr);
}

_Noreturn void novella_platform_panic(unsigned code) {
    (void)code;
    abort();
}

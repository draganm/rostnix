/* A program in C that uses the library; the integration tests link it
   against the installed dynamic library and against the static one. */
#include <stdio.h>

int rostnix_sum(int a, int b);

int main(void) {
    printf("c: %d\n", rostnix_sum(1, 2));
    return 0;
}

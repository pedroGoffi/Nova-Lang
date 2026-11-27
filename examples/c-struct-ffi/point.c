#include <stdio.h>
#include <stdint.h>

typedef struct Point {
    int32_t x;
    double y;
} Point;

void c_point_scale(Point *point, double factor) {
    point->x = (int32_t)(point->x * factor);
    point->y *= factor;

    printf("Hello crom c_point_scale p->x = %d, p->y = %f\n", point->x, point->y);
}

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
typedef struct Replica Replica;
typedef struct { double x; double y; } CrownPoint;
Replica *erl_ios_open(const char *path);
void erl_ios_discover(Replica *replica);
void erl_ios_collections_page(Replica *replica, size_t offset);
bool erl_ios_storage_roots(Replica *replica, const char *roots_json);
void erl_ios_storage_active(Replica *replica, bool active);
bool erl_ios_choose(Replica *replica, const char *root, const char *expected, bool enabled);
// Returns the activity serial acknowledged in status, or 0 if queuing failed.
uint64_t erl_ios_active(Replica *replica, bool active);
uint64_t erl_ios_local_active(Replica *replica);
char *erl_ios_status(const Replica *replica);
void erl_ios_string_free(char *value);
void erl_ios_release(Replica *replica);
bool erl_ios_crown_observe(double seconds, bool working, double *began, double *ended);
size_t erl_ios_crown(double seconds, double began, double ended, bool reduced,
    bool attention, CrownPoint *points, size_t capacity);
// Present only in builds explicitly enabling the synthetic-proof Cargo feature.
char *erl_ios_synthetic_proof(const char *path);
char *erl_ios_reopen_proof(const char *path);

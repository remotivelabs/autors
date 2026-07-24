/*
 * autors-ffi C example: parse an embedded A2L text → query a measurement →
 * read address/convert → update the address → write out → free, printing
 * results along the way.
 *
 * Compiled as the library function autors_c_demo_run into the Rust smoke
 * test (tests/c_smoke.rs); can also be built standalone as an executable:
 *   cc -DAUTORS_DEMO_MAIN -I include examples/c_demo.c -l autors_ffi
 */
#include <stdio.h>
#include <string.h>

#include "autors.h"

static const char *SAMPLE_A2L =
    "/begin PROJECT Demo \"demo project\"\n"
    "/begin MODULE M1 \"module one\"\n"
    "/begin COMPU_METHOD Conv_EngineSpeed \"rpm conv\" LINEAR \"%4.0\" \"rpm\"\n"
    "COEFFS_LINEAR 2 3\n"
    "/end COMPU_METHOD\n"
    "/begin MEASUREMENT EngineSpeed \"engine speed\" UWORD Conv_EngineSpeed 1 0 0 10000\n"
    "ECU_ADDRESS 0x1000\n"
    "/end MEASUREMENT\n"
    "/begin CHARACTERISTIC KFactor \"factor\" VALUE 0x4000 RL_DEFAULT 0 Conv_EngineSpeed 0 100\n"
    "/end CHARACTERISTIC\n"
    "/end MODULE\n"
    "/end PROJECT\n";

/* Append-print into the caller's buffer (snprintf semantics,
 * truncation-safe). */
#define OUT(...)                                                     \
    do {                                                             \
        if (used < buf_len) {                                        \
            int w = snprintf(buf + used, buf_len - used, __VA_ARGS__); \
            if (w > 0) used += (size_t)w;                            \
        }                                                            \
    } while (0)

/* Return value: AUTORS_OK(0) on success; non-zero is the failing step number
 * (buf holds error details). */
int autors_c_demo_run(char *buf, size_t buf_len)
{
    AutorsProject *proj = NULL;
    AutorsMeasurement *meas = NULL;
    AutorsCharacteristic *ch = NULL;
    char *s = NULL;
    char *text = NULL;
    double phys = 0.0;
    double raw = 0.0;
    int rc = 0;
    int step = 0;
    size_t used = 0;

    if (buf_len > 0) buf[0] = '\0';

    /* 1. Parse the embedded A2L text */
    proj = autors_project_parse_string(SAMPLE_A2L);
    if (proj == NULL) {
        OUT("parse failed: %s\n", autors_last_error());
        return 1;
    }
    OUT("modules=%d measurements=%d\n",
        autors_project_module_count(proj),
        autors_project_measurement_count(proj));

    /* 2. Find the measurement; read name/address/data type/conversion name */
    meas = autors_project_find_measurement(proj, "EngineSpeed");
    if (meas == NULL) {
        OUT("find measurement failed: %s\n", autors_last_error());
        step = 2; goto done;
    }
    s = autors_measurement_name(meas);
    OUT("measurement=%s", s != NULL ? s : "?");
    autors_string_free(s);
    OUT(" address=0x%llX", (unsigned long long)autors_measurement_address(meas));
    s = autors_measurement_data_type(meas);
    OUT(" type=%s", s != NULL ? s : "?");
    autors_string_free(s);
    s = autors_measurement_conversion_name(meas);
    OUT(" conversion=%s\n", s != NULL ? s : "?");
    autors_string_free(s);

    /* 3. Find the characteristic; read address/record layout */
    ch = autors_project_find_characteristic(proj, "KFactor");
    if (ch == NULL) {
        OUT("find characteristic failed: %s\n", autors_last_error());
        step = 3; goto done;
    }
    s = autors_characteristic_record_layout(ch);
    OUT("characteristic=KFactor address=0x%llX record_layout=%s\n",
        (unsigned long long)autors_characteristic_address(ch),
        s != NULL ? s : "?");
    autors_string_free(s);

    /* 4. Convert: raw -> phys -> raw */
    rc = autors_measurement_to_physical(proj, "EngineSpeed", 10.0, &phys);
    if (rc != AUTORS_OK) {
        OUT("to_physical failed (%d): %s\n", rc, autors_last_error());
        step = 4; goto done;
    }
    OUT("to_physical(10)=%g\n", phys);
    rc = autors_measurement_to_raw(proj, "EngineSpeed", phys, &raw);
    if (rc != AUTORS_OK) {
        OUT("to_raw failed (%d): %s\n", rc, autors_last_error());
        step = 5; goto done;
    }
    OUT("to_raw(%g)=%g\n", phys, raw);

    /* 5. Update the address and read it back */
    rc = autors_measurement_set_address(proj, "EngineSpeed", 0x2000);
    if (rc != AUTORS_OK) {
        OUT("set_address failed (%d): %s\n", rc, autors_last_error());
        step = 6; goto done;
    }
    meas = autors_project_find_measurement(proj, "EngineSpeed");
    OUT("address_after_set=0x%llX\n",
        (unsigned long long)autors_measurement_address(meas));

    /* 6. Write out the A2L text */
    text = autors_project_write_string(proj);
    if (text == NULL) {
        OUT("write_string failed: %s\n", autors_last_error());
        step = 7; goto done;
    }
    OUT("write_string=%lu bytes\n", (unsigned long)strlen(text));
    autors_string_free(text);
    step = 0;

done:
    /* 7. Free the project */
    autors_project_free(proj);
    return step;
}

#ifdef AUTORS_DEMO_MAIN
int main(void)
{
    static char buf[8192];
    int rc = autors_c_demo_run(buf, sizeof(buf));
    fputs(buf, stdout);
    return rc;
}
#endif

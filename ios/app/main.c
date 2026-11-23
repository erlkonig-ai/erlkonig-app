/* Xcode links this entry point to the Rust application and scene delegates. */
extern int erl_ios_application_main(void);

int main(void) {
    return erl_ios_application_main();
}

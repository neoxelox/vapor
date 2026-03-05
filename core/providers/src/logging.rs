use vapor_shared::logging::GlobalComponentLogger;

static LOGGER: GlobalComponentLogger = GlobalComponentLogger::new("providers", "vapord.logs");

pub fn debug(message: &str, metadata: &[(&str, String)]) {
    LOGGER.debug(message, metadata)
}

pub fn info(message: &str, metadata: &[(&str, String)]) {
    LOGGER.info(message, metadata)
}

pub fn warning(message: &str, metadata: &[(&str, String)]) {
    LOGGER.warning(message, metadata)
}

pub fn error(message: &str, metadata: &[(&str, String)]) {
    LOGGER.error(message, metadata)
}

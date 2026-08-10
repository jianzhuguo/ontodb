package io.ontodb;

/** Base exception for OntoDB SDK errors. */
public class OntoDBException extends Exception {
    public OntoDBException(String message) { super(message); }
    public OntoDBException(String message, Throwable cause) { super(message, cause); }
}

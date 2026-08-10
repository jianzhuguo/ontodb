package io.ontodb;

/** Connection error. */
public class ConnectionException extends OntoDBException {
    public ConnectionException(String message) { super(message); }
    public ConnectionException(String message, Throwable cause) { super(message, cause); }
}

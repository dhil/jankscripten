import * as parser from '@babel/parser';
import * as t from '@babel/types';
import generator from '@babel/generator';
import traverse, { TraverseOptions, NodePath, Visitor } from '@babel/traverse';
import * as process from 'process';
import * as fs from 'fs';
import { memoryUsage } from 'process';

/**
 * An identfier in the generated code. It is bound to the jankyp runtime 
 * module, `runtime.ts`.
 */
const qJankyp = t.identifier('$jankyp');

/**
 * Call a function in the jankyp runtime.
 * @param name name of the runtime function
 * @param args args to the runtime function
 */
function qCall(name: string, args: t.Expression[]): t.CallExpression {
    return t.callExpression(t.memberExpression(qJankyp, t.identifier(name), false), args);
}

/**
 * Converts a babel source location into a generated JS string.
 * @param loc the location to convert
 */
function qLoc(loc: t.SourceLocation): t.Expression {
    return t.stringLiteral(`Line ${loc.start.line}, Column ${loc.start.column}`);
}

/**
 * Sets up a function object to instrument itself. Currently the
 * generated instrumentation checks the functions' arguments for
 * arity mismatches.
 * @param path the babel function object
 */
function instrumentFunction(path: NodePath<t.FunctionDeclaration> | NodePath<t.FunctionExpression>) {
    let numFormals = t.numericLiteral(path.node.params.length);
    let numActuals = t.memberExpression(t.identifier('arguments'), t.identifier('length'), false);
    if (path.node.loc === null) {
        throw new Error('no location');
    }
    path.node.body.body.unshift(t.expressionStatement(qCall('checkArgs', [qLoc(path.node.loc), numFormals, numActuals])));
}

/**
 * The babel visitor, which adds instrumentation functionality to the program's
 * AST.
 */
const visitor: TraverseOptions = {
    Program: {
        exit(path) {
            path.node.body.unshift(
                t.variableDeclaration('const',
                    [t.variableDeclarator(qJankyp,
                        t.callExpression(t.identifier('require'), [t.stringLiteral('./dist/runtime.js')]))]));
        }
    },
    BinaryExpression: {
        exit(path) {
            if (typeof path.node.left.loc === "undefined" || typeof path.node.right.loc === "undefined") {
                return;
            }
            let op = path.node.operator;
            // Let's assume all (in)equalities are safe.
            if (['==', '!=', '===', '!=='].includes(op)) {
                return;
            }
            if (path.node.left.type == 'PrivateName') {
                // No idea what this is.
                return;
            }
            if (path.node.left.loc === null) {
                throw new Error('no location');
            }
            if (path.node.right.loc === null) {
                throw new Error('no location');
            }

            if (['*', '/', '-', '&', '|', '<<', '>>', '>>>'].includes(op)) {
                path.node.left = qCall('expectNumber', [qLoc(path.node.left.loc), path.node.left]);
                path.node.right = qCall('expectNumber', [qLoc(path.node.right.loc), path.node.right]);
                return;
            }
            path.node.left = qCall('checkOperand', [qLoc(path.node.left.loc), path.node.left]);
            path.node.right = qCall('checkOperand', [qLoc(path.node.right.loc), path.node.right]);
        }
    },
    FunctionExpression: {
        exit(path) {
            instrumentFunction(path);
        }
    },
    FunctionDeclaration: {
        exit(path) {
            instrumentFunction(path);
        }
    },
    TryStatement: {
        exit(path) {
            if (path.node.loc === null) {
                return;
            }
            if (path.node.handler !== null) {
                path.node.handler.body.body.unshift(t.expressionStatement(qCall('checkException', [qLoc(path.node.loc)])));
            }
        }
    }
};

// t.isLVal actually returns true for anything that *can* be an LVal
function isLVal(path: NodePath<t.MemberExpression>): boolean {
    let parent = path.parentPath;
    let assign = parent.isAssignmentExpression() && parent.node.left == path.node;
    // TODO(luna): should be able to detect these
    let update = parent.isUpdateExpression();
    return assign || update;
}

function immediatelyCalled(path: NodePath<t.MemberExpression>): boolean {
    let parent = path.parentPath;
    return parent.isCallExpression();
}


function main() {
    let js_str = fs.readFileSync(process.argv[2], { encoding: 'utf-8' });
    let ast = parser.parse(js_str);
    traverse(ast, visitor);

    // test
    let visitors: Visitor[] = [];
    let combinedVisitor = traverse.visitors.merge(visitors);
    traverse(ast, combinedVisitor);
    
    let { code } = generator(ast);
    fs.writeFileSync(process.argv[3], code);
}

main();
